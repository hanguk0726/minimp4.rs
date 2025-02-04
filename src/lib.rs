use libc::malloc;
use minimp4_sys::{
    mp4_h26x_write_init, mp4_h26x_write_nal, mp4_h26x_writer_t, track_media_kind_t_e_audio,
    MP4E_add_track, MP4E_close, MP4E_mux_t, MP4E_open, MP4E_put_sample, MP4E_set_dsi,
    MP4E_set_text_comment, MP4E_track_t, MP4E_track_t__bindgen_ty_1,
    MP4E_track_t__bindgen_ty_1__bindgen_ty_1, MP4E_SAMPLE_RANDOM_ACCESS,
    MP4_OBJECT_TYPE_AUDIO_ISO_IEC_14496_3,
};
use std::convert::TryInto;
use std::ffi::CString;
use std::io::{Seek, SeekFrom, Write};
use std::mem::size_of;
use std::os::raw::c_void;
use std::ptr::null_mut;
use std::slice::from_raw_parts;

use crate::enc::{BitRate, Encoder, EncoderParams};

pub mod enc;
pub struct Mp4Muxer<W> {
    writer: W,
    muxer: *mut MP4E_mux_t,
    muxer_writer: *mut mp4_h26x_writer_t,
    str_buffer: Vec<CString>,
    encoder_params: Option<EncoderParams>,
}

impl<W: Write + Seek> Mp4Muxer<W> {
    pub fn new(writer: W) -> Self {
        unsafe {
            Self {
                writer,
                muxer: null_mut(),
                muxer_writer: malloc(size_of::<mp4_h26x_writer_t>()) as *mut mp4_h26x_writer_t,
                str_buffer: Vec::new(),
                encoder_params: None,
            }
        }
    }

    pub fn init_video(&mut self, width: i32, height: i32, is_hevc: bool, track_name: &str) {
        self.str_buffer.push(CString::new(track_name).unwrap());
        unsafe {
            if self.muxer.is_null() {
                let self_ptr = self as *mut Self as *mut c_void;
                self.muxer = MP4E_open(0, 0, self_ptr, Some(Self::write));
            }
            mp4_h26x_write_init(
                self.muxer_writer,
                self.muxer,
                width,
                height,
                if is_hevc { 1 } else { 0 },
            );
        }
    }

    pub fn init_audio(&mut self, bit_rate: u32, sample_rate: u32, channel_count: u32) {
        self.encoder_params = Some(EncoderParams {
            bit_rate: BitRate::Cbr(bit_rate),
            sample_rate,
            channel_count,
        });
    }
    pub fn write_video(&self, data: &[u8]) {
        self.write_video_with_fps(data, 60);
    }

    pub fn write_video_with_audio(&self, data: &[u8], fps: u32, pcm: &[u8]) {
        assert!(self.encoder_params.is_some());
        let mp4wr = unsafe { self.muxer_writer.as_mut().unwrap() };
        let fps = fps.try_into().unwrap();
        let encoder_params = self.encoder_params.clone().unwrap();
        write_mp4_with_audio(mp4wr, fps, data, pcm, encoder_params)
    }

    pub fn write_video_with_fps(&self, data: &[u8], fps: u32) {
        let mp4wr = unsafe { self.muxer_writer.as_mut().unwrap() };
        let fps = fps.try_into().unwrap();
        write_mp4(mp4wr, fps, data);
    }

    pub fn write_comment(&mut self, comment: &str) {
        self.str_buffer.push(CString::new(comment).unwrap());
        unsafe {
            MP4E_set_text_comment(self.muxer, self.str_buffer.last().unwrap().as_ptr());
        }
    }
    pub fn close(&self) -> &W {
        unsafe {
            MP4E_close(self.muxer);
        }
        &self.writer
    }

    pub fn write_data(&mut self, offset: i64, buf: &[u8]) -> usize {
        self.writer.seek(SeekFrom::Start(offset as u64)).unwrap();
        self.writer.write(buf).unwrap_or(0)
    }

    extern "C" fn write(
        offset: i64,
        buffer: *const c_void,
        size: usize,
        token: *mut c_void,
    ) -> i32 {
        let p_self = token as *mut Self;
        unsafe {
            let buf = from_raw_parts(buffer as *const u8, size);
            ((&mut *p_self).write_data(offset, buf) != size) as i32
        }
    }
}

fn get_nal_size(buf: &mut [u8], size: usize) -> usize {
    let mut pos = 3;
    while size - pos > 3 {
        if buf[pos] == 0 && buf[pos + 1] == 0 && buf[pos + 2] == 1 {
            return pos;
        }
        if buf[pos] == 0 && buf[pos + 1] == 0 && buf[pos + 2] == 0 && buf[pos + 3] == 1 {
            return pos;
        }
        pos += 1;
    }
    size
}

fn write_mp4(mp4wr: &mut mp4_h26x_writer_t, fps: i32, data: &[u8]) {
    let mut data_size = data.len();
    let mut data_ptr = data.as_ptr();

    while data_size > 0 {
        let buf = unsafe { std::slice::from_raw_parts_mut(data_ptr as *mut u8, data_size) };
        let nal_size = get_nal_size(buf, data_size);
        if nal_size < 4 {
            data_ptr = unsafe { data_ptr.add(1) };
            data_size -= 1;
            continue;
        }
        unsafe { mp4_h26x_write_nal(mp4wr, data_ptr, nal_size as i32, (90000 / fps) as u32) };
        data_ptr = unsafe { data_ptr.add(nal_size) };
        data_size -= nal_size;
    }
}

fn write_mp4_with_audio(
    mp4wr: &mut mp4_h26x_writer_t,
    fps: i32,
    data: &[u8],
    pcm: &[u8],
    encoder_params: EncoderParams,
) {
    let mut data_size = data.len();
    let mut data_ptr = data.as_ptr();

    let sample_rate = encoder_params.sample_rate;
    let channel_count = encoder_params.channel_count;

    let encoder = Encoder::new(encoder_params).unwrap();
    let info = encoder.info().unwrap();

    let language: [u8; 4] = [0x75, 0x6e, 0x64, 0x00]; // und\0
    let tr: MP4E_track_t = MP4E_track_t {
        object_type_indication: MP4_OBJECT_TYPE_AUDIO_ISO_IEC_14496_3,
        language: language,
        track_media_kind: track_media_kind_t_e_audio,
        time_scale: 90000,
        default_duration: 0,
        u: MP4E_track_t__bindgen_ty_1 {
            a: MP4E_track_t__bindgen_ty_1__bindgen_ty_1 {
                channelcount: channel_count,
            },
        },
    };

    let mux = mp4wr.mux;
    let audio_track_id = unsafe { MP4E_add_track(mux, &tr) };

    unsafe {
        MP4E_set_dsi(
            mux,
            audio_track_id,
            info.confBuf.as_ptr() as *const c_void,
            info.confSize.try_into().unwrap(),
        )
    };

    let length: u64 = if channel_count == 1 { 1024 } else { 2048 };
    let mut input_buffer = vec![0i16; length as usize];
    let mut output_buffer = vec![0u8; length as usize];

    let pcm_size: u64 = pcm.len().try_into().unwrap();

    let mut sample: u64 = 0;
    let mut total_samples: u64 = pcm_size ;
    let mut ts: u64 = 0;
    let mut ats: u64 = 0;

    let in_args_num_in_samples = length;
    let mut pcm_ptr = pcm.as_ptr();

    while data_size > 0 {
        let buf = unsafe { std::slice::from_raw_parts_mut(data_ptr as *mut u8, data_size) };
        let nal_size = get_nal_size(buf, data_size);
        if nal_size < 4 {
            data_ptr = unsafe { data_ptr.add(1) };
            data_size -= 1;
            continue;
        }
        unsafe { mp4_h26x_write_nal(mp4wr, data_ptr, nal_size as i32, (90000 / fps) as u32) };
        data_ptr = unsafe { data_ptr.add(nal_size) };
        data_size -= nal_size;

        ts += 90000 / fps as u64;
        while ats < ts {
            let bytes_to_read = std::cmp::min(total_samples, in_args_num_in_samples as u64);
            let bytes_read = bytes_to_read * 2; // 2 bytes per i16
            let pcm_buf = unsafe {
                std::slice::from_raw_parts(pcm_ptr as *const i16, bytes_to_read.try_into().unwrap())
            };
            // Copy PCM data into input buffer
            input_buffer[..bytes_to_read as usize].copy_from_slice(pcm_buf);
            pcm_ptr = unsafe { pcm_ptr.add(bytes_read.try_into().unwrap()) };

            // if total_samples < in_args_num_in_samples as u64 {
            //     total_samples = pcm_size ;
            // }

            // Encode audio data using AAC encoder
            match encoder.encode(&input_buffer[..bytes_to_read as usize], &mut output_buffer) {
                Ok(encoding_info) => {
                    // Write encoded audio data to output buffer
                    let buf = &output_buffer[..encoding_info.output_size];
                    sample += 1024;
                    // total_samples -= bytes_to_read;
                    ats = sample * 90000 / sample_rate as u64;
                    unsafe {
                        MP4E_put_sample(
                            mux,
                            audio_track_id,
                            buf.as_ptr() as *mut c_void,
                            encoding_info.output_size.try_into().unwrap(),
                            (1024 * 90000 / sample_rate as usize).try_into().unwrap(),
                            MP4E_SAMPLE_RANDOM_ACCESS.try_into().unwrap(),
                        )
                    };

                }
                Err(e) => {
                    println!("encode error:{}", e);
                    break;
                }
            }
        }
    }
}
