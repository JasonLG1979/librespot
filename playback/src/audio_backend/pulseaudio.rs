use super::{Open, Sink, SinkAsBytes};
use crate::config::AudioFormat;
use crate::convert::Converter;
use crate::decoder::AudioPacket;
use crate::{NUM_CHANNELS, SAMPLES_PER_SECOND, SAMPLE_RATE};
use libpulse_binding::{self as pulse, def::BufferAttr, error::PAErr, stream::Direction};
use libpulse_simple_binding::Simple;
use std::cmp::min;
use std::io;
use thiserror::Error;

const APP_NAME: &str = "librespot";
const STREAM_NAME: &str = "Spotify endpoint";

#[derive(Debug, Error)]
enum PulseError {
    #[error("Error starting PulseAudioSink, invalid PulseAudio sample spec")]
    InvalidSampleSpec,
    #[error("Error starting PulseAudioSink, could not connect to PulseAudio server, {0}")]
    ConnectionRefused(PAErr),
    #[error("Error stopping PulseAudioSink, failed to drain PulseAudio server buffer, {0}")]
    DrainFailure(PAErr),
    #[error("Error in PulseAudioSink, Not connected to PulseAudio server")]
    NotConnected,
    #[error("Error writing from PulseAudioSink to PulseAudio server, {0}")]
    OnWrite(PAErr),
}

impl From<PulseError> for io::Error {
    fn from(e: PulseError) -> io::Error {
        use PulseError::*;

        match e {
            DrainFailure(_) | OnWrite(_) => io::Error::new(io::ErrorKind::BrokenPipe, e),
            ConnectionRefused(_) => io::Error::new(io::ErrorKind::ConnectionRefused, e),
            InvalidSampleSpec => io::Error::new(io::ErrorKind::InvalidInput, e),
            NotConnected => io::Error::new(io::ErrorKind::NotConnected, e),
        }
    }
}

pub struct PulseAudioSink {
    sink: Option<Simple>,
    device: Option<String>,
    format: AudioFormat,
    buffer: Vec<u8>,
}

impl Open for PulseAudioSink {
    fn open(device: Option<String>, format: AudioFormat) -> Self {
        let mut actual_format = format;

        if actual_format == AudioFormat::F64 {
            warn!("PulseAudio currently does not support F64 output");
            actual_format = AudioFormat::F32;
        }

        info!("Using PulseAudioSink with format: {:?}", actual_format);

        Self {
            sink: None,
            device,
            format: actual_format,
            buffer: vec![],
        }
    }
}

impl Sink for PulseAudioSink {
    fn start(&mut self) -> io::Result<()> {
        if self.sink.is_none() {
            // PulseAudio calls S24 and S24_3 different from the rest of the world
            let pulse_format = match self.format {
                AudioFormat::F32 => pulse::sample::Format::FLOAT32NE,
                AudioFormat::S32 => pulse::sample::Format::S32NE,
                AudioFormat::S24 => pulse::sample::Format::S24_32NE,
                AudioFormat::S24_3 => pulse::sample::Format::S24NE,
                AudioFormat::S16 => pulse::sample::Format::S16NE,
                _ => unreachable!(),
            };

            let ss = pulse::sample::Spec {
                format: pulse_format,
                channels: NUM_CHANNELS,
                rate: SAMPLE_RATE,
            };

            if !ss.is_valid() {
                return Err(io::Error::from(PulseError::InvalidSampleSpec));
            }

            // 100ms of audio in bytes.
            let buffer_size_bytes = (SAMPLES_PER_SECOND * self.format.size() as u32) / 10;

            if self.buffer.capacity() > buffer_size_bytes as usize {
                self.buffer.shrink_to_fit();
            }

            self.buffer
                .reserve_exact(buffer_size_bytes as usize - self.buffer.len());

            // Basically match what we do in the alsa backend.
            // u32::MAX = the default.
            let buffer_attr = BufferAttr {
                // Total buffer size 500ms.
                maxlength: buffer_size_bytes * 5,
                tlength: u32::MAX,
                // prebuf is the same as set_start_threshold
                // in the alsa backend. Buffer 400ms of audio
                // before starting playback.
                prebuf: buffer_size_bytes * 4,
                minreq: u32::MAX,
                fragsize: u32::MAX,
            };

            let sink = Simple::new(
                None,                   // Use the default server.
                APP_NAME,               // Our application's name.
                Direction::Playback,    // Direction.
                self.device.as_deref(), // Our device (sink) name.
                STREAM_NAME,            // Description of our stream.
                &ss,                    // Our sample format.
                None,                   // Use default channel map.
                Some(&buffer_attr),     // Use our buffering attributes.
            )
            .map_err(PulseError::ConnectionRefused)?;

            self.sink = Some(sink);
        }

        Ok(())
    }

    fn stop(&mut self) -> io::Result<()> {
        self.buffer.resize(self.buffer.capacity(), 0);
        self.write_buf()?;

        let sink = self.sink.as_mut().ok_or(PulseError::NotConnected)?;

        sink.drain().map_err(PulseError::DrainFailure)?;

        self.sink = None;
        Ok(())
    }

    sink_as_bytes!();
}

impl SinkAsBytes for PulseAudioSink {
    fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        let mut start_index = 0;
        let data_len = data.len();
        let capacity = self.buffer.capacity();

        loop {
            let data_left = data_len - start_index;
            let space_left = capacity - self.buffer.len();
            let data_to_buffer = min(data_left, space_left);
            let end_index = start_index + data_to_buffer;

            self.buffer.extend_from_slice(&data[start_index..end_index]);

            if self.buffer.len() == capacity {
                self.write_buf()?;
            }

            if end_index == data_len {
                break Ok(());
            }

            start_index = end_index;
        }
    }
}

impl PulseAudioSink {
    pub const NAME: &'static str = "pulseaudio";

    fn write_buf(&mut self) -> Result<(), PulseError> {
        let sink = self.sink.as_mut().ok_or(PulseError::NotConnected)?;

        sink.write(&self.buffer).map_err(PulseError::OnWrite)?;

        self.buffer.clear();
        Ok(())
    }
}
