use crossbeam_channel::{Receiver, Sender, unbounded};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::StreamConfig;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Debug)]
enum PlayerCommand {
    Play(String),
    Stop,
    TogglePause,
    SetVolume(f32),
    Quit,
}

/// Shared between UI and the audio thread (volume, pause, last error).
#[derive(Debug)]
pub struct PlayerState {
    last_error: Mutex<Option<String>>,
    volume: Mutex<f32>,
    paused: AtomicBool,
}

impl PlayerState {
    fn new() -> Self {
        Self {
            last_error: Mutex::new(None),
            volume: Mutex::new(1.0_f32),
            paused: AtomicBool::new(false),
        }
    }

    pub fn take_last_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.take()
    }

    fn set_error(&self, msg: impl Into<String>) {
        if let Ok(mut g) = self.last_error.lock() {
            *g = Some(msg.into());
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume.lock().map(|g| *g).unwrap_or(1.0).clamp(0.0, 1.0)
    }

    pub fn set_volume(&self, v: f32) {
        if let Ok(mut g) = self.volume.lock() {
            *g = v.clamp(0.0, 1.0);
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }
}

struct HttpBufferedSource {
    inner: Mutex<HttpBufferedInner>,
}

struct HttpBufferedInner {
    resp: reqwest::blocking::Response,
    buf: Vec<u8>,
    pos: u64,
    eof: bool,
}

impl HttpBufferedInner {
    fn read_until(&mut self, target_len: usize) -> io::Result<()> {
        while self.buf.len() < target_len && !self.eof {
            let mut tmp = [0u8; 8192];
            let n = self.resp.read(&mut tmp)?;
            if n == 0 {
                self.eof = true;
                break;
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }
}

impl HttpBufferedSource {
    fn new(resp: reqwest::blocking::Response) -> Self {
        Self {
            inner: Mutex::new(HttpBufferedInner {
                resp,
                buf: Vec::new(),
                pos: 0,
                eof: false,
            }),
        }
    }
}

impl Read for HttpBufferedSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().expect("http buffer lock poisoned");
        let start = inner.pos as usize;
        let target = start.saturating_add(out.len());
        inner.read_until(target)?;
        let available = inner.buf.len().saturating_sub(start).min(out.len());
        if available == 0 {
            return Ok(0);
        }
        out[..available].copy_from_slice(&inner.buf[start..start + available]);
        inner.pos += available as u64;
        Ok(available)
    }
}

impl Seek for HttpBufferedSource {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let mut inner = self.inner.lock().expect("http buffer lock poisoned");
        let new_pos = match pos {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(n) => inner.pos as i64 + n,
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Seek from end unsupported for stream",
                ))
            }
        };
        if new_pos < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "negative seek"));
        }
        let new_pos_u = new_pos as u64;
        inner.read_until(new_pos_u as usize)?;
        inner.pos = new_pos_u;
        Ok(inner.pos)
    }
}

impl MediaSource for HttpBufferedSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

struct StreamDecoder {
    reader: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: u16,
}

impl StreamDecoder {
    fn open_url(url: &str) -> anyhow::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()?;
        let resp = client.get(url).send()?;
        if !resp.status().is_success() {
            anyhow::bail!("stream HTTP {}", resp.status());
        }

        let mut hint = Hint::new();
        if let Some(path) = url.split('?').next() {
            if let Some(ext) = path.rsplit('.').next() {
                if !ext.is_empty() && ext.len() <= 6 {
                    hint.with_extension(ext);
                }
            }
        }

        let source = HttpBufferedSource::new(resp);
        let mss = MediaSourceStream::new(Box::new(source), Default::default());

        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )?;

        let reader = probed.format;
        let track = reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| anyhow::anyhow!("no audio track"))?;
        let track_id = track.id;
        let codec_params = track.codec_params.clone();
        let sample_rate = codec_params
            .sample_rate
            .ok_or_else(|| anyhow::anyhow!("unknown sample rate"))?;
        let channels = codec_params.channels.map(|c| c.count() as u16).unwrap_or(2);

        let decoder = symphonia::default::get_codecs().make(&codec_params, &DecoderOptions::default())?;
        Ok(Self {
            reader,
            decoder,
            track_id,
            sample_rate,
            channels,
        })
    }

    fn next_samples(&mut self) -> anyhow::Result<Option<Vec<f32>>> {
        loop {
            let packet = match self.reader.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(anyhow::anyhow!(e.to_string())),
            };
            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(anyhow::anyhow!(e.to_string())),
            };

            let spec = *decoded.spec();
            let mut sample_buf = SampleBuffer::<f32>::new(decoded.frames() as u64, spec);
            sample_buf.copy_interleaved_ref(decoded);
            return Ok(Some(sample_buf.samples().to_vec()));
        }
    }
}

#[derive(Debug)]
pub struct RadioPlayer {
    tx: Sender<PlayerCommand>,
    pub state: Arc<PlayerState>,
}

impl RadioPlayer {
    pub fn new() -> Self {
        let state = Arc::new(PlayerState::new());
        let state_thread = Arc::clone(&state);
        let (tx, rx) = unbounded::<PlayerCommand>();
        thread::Builder::new()
            .name("openrustmap-radio-player".into())
            .spawn(move || run_audio_thread(rx, state_thread))
            .ok();

        Self { tx, state }
    }

    pub fn play(&self, url: String) {
        let _ = self.tx.send(PlayerCommand::Play(url));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(PlayerCommand::Stop);
    }

    pub fn toggle_pause(&self) {
        let _ = self.tx.send(PlayerCommand::TogglePause);
    }

    pub fn set_volume_command(&self, v: f32) {
        let _ = self.tx.send(PlayerCommand::SetVolume(v));
    }
}

impl Drop for RadioPlayer {
    fn drop(&mut self) {
        let _ = self.tx.send(PlayerCommand::Quit);
    }
}

fn run_audio_thread(rx: Receiver<PlayerCommand>, state: Arc<PlayerState>) {
    let host = cpal::default_host();
    let Some(device) = host.default_output_device() else {
        state.set_error("No default audio output device.");
        return;
    };

    let mut stop_flag: Option<Arc<AtomicBool>> = None;
    let mut active: Option<ActiveStream> = None;

    loop {
        while let Ok(cmd) = rx.try_recv() {
            if apply_command(
                cmd,
                &mut stop_flag,
                &mut active,
                &device,
                &state,
                &rx,
            ) {
                return;
            }
        }

        if let Some(a) = &mut active {
            if a.local_stop.load(Ordering::Acquire) {
                active = None;
                stop_flag = None;
                continue;
            }

            if state.paused.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(8));
                continue;
            }

            if a.producer.slots() > 4096 {
                match a.decoder.next_samples() {
                    Ok(Some(samples)) => {
                        let n = samples.len().min(a.producer.slots());
                        for &s in &samples[..n] {
                            let _ = a.producer.push(s);
                        }
                    }
                    Ok(None) => {
                        active = None;
                        stop_flag = None;
                    }
                    Err(e) => {
                        state.set_error(format!("Decode error: {e}"));
                        active = None;
                        stop_flag = None;
                    }
                }
            } else {
                thread::sleep(Duration::from_millis(5));
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(cmd) => {
                    if apply_command(
                        cmd,
                        &mut stop_flag,
                        &mut active,
                        &device,
                        &state,
                        &rx,
                    ) {
                        return;
                    }
                }
                Err(_) => {}
            }
        }
    }
}

struct ActiveStream {
    producer: rtrb::Producer<f32>,
    decoder: StreamDecoder,
    local_stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    stream: cpal::Stream,
}

fn apply_command(
    cmd: PlayerCommand,
    stop_flag: &mut Option<Arc<AtomicBool>>,
    active: &mut Option<ActiveStream>,
    device: &cpal::Device,
    state: &Arc<PlayerState>,
    rx: &Receiver<PlayerCommand>,
) -> bool {
    match cmd {
        PlayerCommand::Quit => return true,
        PlayerCommand::Stop => {
            if let Some(flag) = stop_flag {
                flag.store(true, Ordering::Release);
            }
            *stop_flag = None;
            *active = None;
            state.paused.store(false, Ordering::Release);
        }
        PlayerCommand::TogglePause => {
            let p = !state.paused.load(Ordering::Acquire);
            state.paused.store(p, Ordering::Release);
        }
        PlayerCommand::SetVolume(v) => {
            state.set_volume(v);
        }
        PlayerCommand::Play(url) => {
            if let Some(flag) = stop_flag {
                flag.store(true, Ordering::Release);
            }
            *stop_flag = None;
            *active = None;
            state.paused.store(false, Ordering::Release);

            let decoder = match StreamDecoder::open_url(&url) {
                Ok(d) => d,
                Err(e) => {
                    state.set_error(format!("Could not open stream: {e}"));
                    return false;
                }
            };
            let ring_capacity = (decoder.sample_rate as usize) * (decoder.channels as usize);
            let (producer, mut consumer) = rtrb::RingBuffer::<f32>::new(ring_capacity.max(4096));
            let local_stop = Arc::new(AtomicBool::new(false));
            let stop_ref = Arc::clone(&local_stop);
            let state_out = Arc::clone(state);
            let state_err = Arc::clone(state);

            let config = StreamConfig {
                channels: decoder.channels,
                sample_rate: cpal::SampleRate(decoder.sample_rate),
                buffer_size: cpal::BufferSize::Default,
            };

            let stream = match device.build_output_stream(
                &config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if stop_ref.load(Ordering::Acquire) {
                        data.fill(0.0);
                        return;
                    }
                    let vol = state_out.volume();
                    if state_out.paused.load(Ordering::Acquire) {
                        data.fill(0.0);
                        return;
                    }
                    for dst in data.iter_mut() {
                        *dst = consumer.pop().unwrap_or(0.0) * vol;
                    }
                },
                move |err| {
                    state_err.set_error(format!("Audio output: {err}"));
                },
                None,
            ) {
                Ok(s) => s,
                Err(e) => {
                    state.set_error(format!("Could not open audio output: {e}"));
                    return false;
                }
            };
            if let Err(e) = stream.play() {
                state.set_error(format!("Could not start playback: {e}"));
                return false;
            }
            *stop_flag = Some(Arc::clone(&local_stop));
            *active = Some(ActiveStream {
                producer,
                decoder,
                local_stop,
                stream,
            });

            while let Ok(more) = rx.try_recv() {
                if apply_command(more, stop_flag, active, device, state, rx) {
                    return true;
                }
            }
        }
    }
    false
}
