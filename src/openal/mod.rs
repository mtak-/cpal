extern crate alto;

use self::alto::{
    Alto, AltoError, Buffer, Context, DeviceObject, Mono, OutputDevice, SampleFrame, Source,
    SourceState, Stereo, StreamingSource,
};
use std::{
    ffi::CStr,
    marker::PhantomData,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
    vec::IntoIter,
};

use CreationError;
use DefaultFormatError;
use Format;
use FormatsEnumerationError;
use OutputBuffer as RootOutputBuffer;
use Sample;
use SampleFormat;
use SampleRate;
use StreamData;
use SupportedFormat;
use UnknownTypeOutputBuffer;

lazy_static! {
    static ref ALTO: Alto = Alto::load_default().expect("failed to load OpenAL");
}

const POLL_TIME: Duration = Duration::from_millis(4);
const BUF_SIZE: usize = 44_100 * 2 / 10;

#[derive(Clone, PartialEq, Eq)]
pub struct Device {
    output_device: Arc<OutputDevice>,
    context: Context,
}

impl Device {
    fn from_specifier(specifier: Option<&CStr>) -> Result<Self, AltoError> {
        let output_device = Arc::new(ALTO.open(specifier)?);
        let context = output_device.new_context(None)?;
        Ok(Device {
            output_device,
            context,
        })
    }

    pub fn name(&self) -> String {
        self.output_device
            .specifier()
            .map(|specifier| specifier.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown OpenAL device".to_owned())
    }

    pub fn supported_input_formats(
        &self,
    ) -> Result<SupportedInputFormats, FormatsEnumerationError> {
        unimplemented!()
    }

    pub fn supported_output_formats(
        &self,
    ) -> Result<SupportedOutputFormats, FormatsEnumerationError> {
        Ok(SupportedOutputFormats(
            vec![
                SupportedFormat {
                    channels: 1,
                    data_type: SampleFormat::I16,
                    min_sample_rate: SampleRate(0),
                    max_sample_rate: SampleRate(44100),
                },
                // SupportedFormat {
                //     channels: 2,
                //     data_type: SampleFormat::I16,
                //     min_sample_rate: SampleRate(0),
                //     max_sample_rate: SampleRate(44100),
                // },
                // SupportedFormat {
                //     channels: 1,
                //     data_type: SampleFormat::F32,
                //     min_sample_rate: SampleRate(0),
                //     max_sample_rate: SampleRate(44100),
                // },
                // SupportedFormat {
                //     channels: 2,
                //     data_type: SampleFormat::F32,
                //     min_sample_rate: SampleRate(0),
                //     max_sample_rate: SampleRate(44100),
                // },
            ]
            .into_iter(),
        ))
    }

    pub fn default_input_format(&self) -> Result<Format, DefaultFormatError> {
        unimplemented!()
    }

    pub fn default_output_format(&self) -> Result<Format, DefaultFormatError> {
        Ok(Format {
            channels: 2,
            data_type: SampleFormat::I16,
            sample_rate: SampleRate(44_100),
        })
    }
}

pub struct Devices {
    devices: IntoIter<Device>,
}

impl Default for Devices {
    fn default() -> Devices {
        let devices = ALTO
            .enumerate_outputs()
            .into_iter()
            .map(|specifier| Device::from_specifier(Some(&specifier)))
            .collect::<Result<Vec<_>, AltoError>>()
            .unwrap_or_else(|_| Vec::new())
            .into_iter();
        Devices { devices }
    }
}

impl Iterator for Devices {
    type Item = Device;

    fn next(&mut self) -> Option<Device> {
        self.devices.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.devices.size_hint()
    }
}

pub fn default_input_device() -> Option<Device> {
    unimplemented!()
}

pub fn default_output_device() -> Option<Device> {
    Device::from_specifier(None).ok()
}

pub struct EventLoop {
    streams: Mutex<Vec<Option<StreamInner>>>,
    waiting: Condvar,
}

impl EventLoop {
    pub fn new() -> EventLoop {
        EventLoop {
            streams: Default::default(),
            waiting: Condvar::new(),
        }
    }

    pub fn build_input_stream(
        &self, _device: &Device, _format: &Format,
    ) -> Result<StreamId, CreationError> {
        unimplemented!()
    }

    pub fn build_output_stream(
        &self, device: &Device, format: &Format,
    ) -> Result<StreamId, CreationError> {
        let mut streams = self.streams.lock().unwrap();
        let p = streams.iter().position(|s| s.is_none()).unwrap_or_else(|| {
            streams.push(None);
            streams.len() - 1
        });
        let streaming_source = device
            .context
            .new_streaming_source()
            .map_err(|_| CreationError::FormatNotSupported)?;
        streams[p] = Some(StreamInner {
            streaming_source,
            format: format.clone(),
            sample_len: 0,
        });
        println!("{:?} build", StreamId(p));
        self.waiting.notify_one();
        Ok(StreamId(p))
    }

    pub fn play_stream(&self, stream: StreamId) {
        let mut streams = self.streams.lock().unwrap();
        streams[stream.0].as_mut().unwrap().streaming_source.play();
        println!("{:?} play", stream);
        self.waiting.notify_one();
    }

    pub fn pause_stream(&self, stream: StreamId) {
        let mut streams = self.streams.lock().unwrap();
        streams[stream.0].as_mut().unwrap().streaming_source.pause();
        println!("{:?} pause", stream);
        self.waiting.notify_one();
    }

    pub fn destroy_stream(&self, stream: StreamId) {
        let mut streams = self.streams.lock().unwrap();
        streams[stream.0] = None;
        println!("{:?} destroy", stream);
        self.waiting.notify_one();
    }

    pub fn run<F>(&self, mut callback: F) -> !
    where
        F: FnMut(StreamId, StreamData) + Send,
    {
        let mut streams = self.streams.lock().unwrap();
        loop {
            for (stream_id, stream) in streams
                .iter_mut()
                .enumerate()
                .filter_map(|(pos, s)| s.as_mut().map(move |s| (StreamId(pos), s)))
            {
                let state = stream.streaming_source.state();
                println!("State: {:?}", state);
                if state == SourceState::Initial
                    || state == SourceState::Stopped
                    || stream.streaming_source.buffers_processed() > 0
                    || stream.streaming_source.buffers_queued() < 2
                {
                    let stream_data = match stream.format {
                        Format {
                            data_type: SampleFormat::I16,
                            ..
                        } => UnknownTypeOutputBuffer::I16(RootOutputBuffer {
                            target: Some(OutputBuffer {
                                data: vec![0; BUF_SIZE],
                                stream_inner: stream,
                            }),
                        }),
                        Format {
                            data_type: SampleFormat::F32,
                            ..
                        } => UnknownTypeOutputBuffer::F32(RootOutputBuffer {
                            target: Some(OutputBuffer {
                                data: vec![0.0; BUF_SIZE],
                                stream_inner: stream,
                            }),
                        }),
                        _ => unimplemented!(),
                    };
                    callback(
                        stream_id,
                        StreamData::Output {
                            buffer: stream_data,
                        },
                    )
                }
            }

            streams = self.waiting.wait_timeout(streams, POLL_TIME).unwrap().0;
        }
    }
}

pub struct StreamInner {
    streaming_source: StreamingSource,
    sample_len: i32,
    format: Format,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
pub struct StreamId(usize);

pub struct InputBuffer<'a, T: 'a>(PhantomData<&'a T>);

impl<'a, T: 'a + Sample> InputBuffer<'a, T> {
    pub fn buffer(&self) -> &[T] {
        unimplemented!()
    }

    pub fn finish(self) {
        unimplemented!()
    }
}

pub struct OutputBuffer<'a, T: 'a> {
    data: Vec<T>,
    stream_inner: &'a mut StreamInner,
}

impl<'a, T: 'a + Sample> OutputBuffer<'a, T> {
    pub fn buffer(&mut self) -> &mut [T] {
        &mut self.data
    }

    pub fn len(&self) -> usize {
        self.data.len() / self.stream_inner.format.channels as usize
    }

    pub fn finish(self) {
        let raw = self.data.as_ptr();
        let state = self.stream_inner.streaming_source.state();

        let buf = {
            let sub = (state == SourceState::Playing) as alto::sys::ALint;
            let old_buf = (0 .. (self.stream_inner.streaming_source.buffers_queued() - sub).max(0))
                .map(|_| {
                    let old_buf = self.stream_inner.streaming_source.unqueue_buffer().unwrap();
                    let size = old_buf.size();
                    let chan = old_buf.channels();
                    let bit_depth = old_buf.bits();
                    self.stream_inner.sample_len = self
                        .stream_inner
                        .sample_len
                        .checked_sub(size / (chan * bit_depth / 8))
                        .unwrap();
                    old_buf
                })
                .last();

            fn publish_buffer_data<'a, F: SampleFrame, T: 'a + Sample>(
                old_buf: Option<Buffer>, raw: *const F, this: &OutputBuffer<'a, T>,
            ) -> Buffer {
                let data = unsafe { std::slice::from_raw_parts(raw, this.len()) };
                let sample_rate = this.stream_inner.format.sample_rate.0 as _;
                old_buf.map_or_else(
                    || {
                        this.stream_inner
                            .streaming_source
                            .context()
                            .new_buffer(data, sample_rate)
                            .expect("failed to create a new OpenAL buffer")
                    },
                    |mut buf| {
                        buf.set_data(data, sample_rate)
                            .expect("failed to set OpenAL buffer data");
                        buf
                    },
                )
            }

            match self.stream_inner.format {
                Format {
                    channels: 1,
                    data_type: SampleFormat::I16,
                    ..
                } => {
                    let raw = raw as *const Mono<i16>;
                    publish_buffer_data(old_buf, raw, &self)
                },
                Format {
                    channels: 1,
                    data_type: SampleFormat::F32,
                    ..
                } => {
                    let raw = raw as *const Mono<f32>;
                    publish_buffer_data(old_buf, raw, &self)
                },
                Format {
                    channels: 2,
                    data_type: SampleFormat::I16,
                    ..
                } => {
                    let raw = raw as *const Stereo<i16>;
                    publish_buffer_data(old_buf, raw, &self)
                },
                Format {
                    channels: 2,
                    data_type: SampleFormat::F32,
                    ..
                } => {
                    let raw = raw as *const Stereo<f32>;
                    publish_buffer_data(old_buf, raw, &self)
                },
                _ => unimplemented!(),
            }
        };

        self.stream_inner.sample_len = self
            .stream_inner
            .sample_len
            .checked_add(self.len() as _)
            .expect("overflowed the sample count");
        self.stream_inner
            .streaming_source
            .queue_buffer(buf)
            .expect("failed to queue alto buffer");
        if state == SourceState::Initial || state == SourceState::Stopped {
            println!("playing from finish {:?}", state);
            self.stream_inner.streaming_source.play();
        }
        println!("finished buffer");
    }
}

pub struct SupportedInputFormats(IntoIter<SupportedFormat>);

impl Iterator for SupportedInputFormats {
    type Item = SupportedFormat;

    fn next(&mut self) -> Option<SupportedFormat> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

pub struct SupportedOutputFormats(IntoIter<SupportedFormat>);

impl Iterator for SupportedOutputFormats {
    type Item = SupportedFormat;

    fn next(&mut self) -> Option<SupportedFormat> {
        self.0.next()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
