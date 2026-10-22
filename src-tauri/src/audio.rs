// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Tier 3's ears (sound.rs runs them): a song decoded (a preview's MP3 or
//! AAC, or your own file: FLAC, ALAC, MP3, AAC, WAV, AIFF, Ogg Vorbis),
//! brought to 16 kHz mono, turned into the mel bands the models read, and
//! heard by both models (Discogs-EffNet, from the Music Technology Group at
//! Universitat Pompeu Fabra, essentia.upf.edu/models). Each step is what
//! Essentia, the makers' own library, does in their examples for these
//! models, sample for sample; nothing here touches the network, and the
//! disk only for the models and a file being heard.

use std::f64::consts::PI;
use std::path::Path;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use tract_onnx::prelude::*;

/// The models hear 16 kHz.
pub const RATE: u32 = 16_000;
/// Frames of 512 samples, 256 apart (16 ms).
const FRAME: usize = 512;
const HOP: usize = 256;
pub const BANDS: usize = 96;
/// The models take patches of 128 frames (about 2 s), 62 frames apart
/// (about 1 s).
const PATCH: usize = 128;
const PATCH_HOP: usize = 62;
/// At most this many patches of a song are heard (a preview has 29).
const MOST_PATCHES: usize = 64;
/// Every other patch is heard: one every 2 s (15 of a 30-second preview)
/// gives about the same print as all 29 in half the time, and four
/// previews fill one of the artist model's batches of 64.
pub const PATCH_EVERY: usize = 2;
/// The artist model takes exactly this many patches at a time.
pub const ARTIST_BATCH: usize = 64;
/// A whole song (your own file) is heard at 32 patches spread over it (one
/// every 9 s of a 5-minute song): about what all of them would give, for
/// twice a preview's time, and two songs fill one of the artist model's
/// batches.
pub const SONG_PATCHES: usize = 32;
/// Of a longer file, the first 20 minutes are heard.
const MOST_SECONDS: usize = 20 * 60;
pub const STYLES: usize = 400;
pub const ARTIST_DIMS: usize = 512;

type Plan = Arc<TypedRunnableModel>;

/// Both models, ready to run.
pub struct Models {
    style: Plan,
    artist: Plan,
}

/// What a song sounds like to the models, averaged over its patches: where
/// it sits among artists (the artist model's embedding, unit length; songs
/// that sound alike point the same way) and how much it sounds like each
/// of the 400 styles (the style model's activations, 0 to 1).
#[derive(Debug, Clone)]
pub struct Heard {
    pub artist: Vec<f32>,
    pub styles: Vec<f32>,
}

impl Models {
    /// Reads the two models (ONNX). Takes a moment: run it off the async
    /// threads.
    pub fn load(style: &Path, artist: &Path) -> Result<Models, String> {
        let plan = |path: &Path| -> Result<Plan, String> {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            tract_onnx::onnx()
                .model_for_path(path)
                .and_then(|m| m.into_optimized())
                .and_then(|m| m.into_runnable())
                .map_err(|e| format!("{name} didn't load: {e}"))
        };
        Ok(Models { style: plan(style)?, artist: plan(artist)? })
    }

    /// Hears several songs together, each song's print in their order: the
    /// style model once over all their patches (its batch is open), the
    /// artist model in batches of 64 filled with them (the last filled out
    /// with silence: zeros, log10(1 + 0), its rows left out). Takes a few
    /// seconds: run it off the async threads.
    pub fn hear_many(&self, songs: &[Patches]) -> Vec<Result<Heard, String>> {
        let n: usize = songs.iter().map(|s| s.count).sum();
        let rows = if n == 0 { Err("too short to hear".to_string()) } else { self.run_both(songs, n) };
        let (style_rows, artist_rows) = match rows {
            Ok(rows) => rows,
            Err(e) => return songs.iter().map(|_| Err(e.clone())).collect(),
        };
        let mut at = 0;
        songs
            .iter()
            .map(|s| {
                if s.count == 0 {
                    return Err("too short to hear".to_string());
                }
                let (from, to) = (at, at + s.count);
                at = to;
                let styles = mean(&style_rows[from..to], STYLES);
                let mut artist = mean(&artist_rows[from..to], ARTIST_DIMS);
                let length = artist.iter().map(|v| v * v).sum::<f32>().sqrt();
                if !(length > 0.0) {
                    return Err("heard nothing".to_string());
                }
                artist.iter_mut().for_each(|v| *v /= length);
                Ok(Heard { artist, styles })
            })
            .collect()
    }

    /// Both models over the songs' patches (`n` in all): each patch's style
    /// activations and artist embedding.
    fn run_both(&self, songs: &[Patches], n: usize) -> Result<(Vec<Vec<f32>>, Vec<Vec<f32>>), String> {
        let size = PATCH * BANDS;
        let mut all: Vec<f32> = Vec::with_capacity(n * size);
        for s in songs {
            all.extend_from_slice(&s.data);
        }
        let input: Tensor = tract_ndarray::Array3::from_shape_vec((n, PATCH, BANDS), all.clone()).map_err(|e| e.to_string())?.into();
        let out = self.style.run(tvec!(input.into())).map_err(|e| format!("The style model stopped: {e}"))?;
        let activations = out[0].to_plain_array_view::<f32>().map_err(|e| e.to_string())?;
        let style_rows: Vec<Vec<f32>> = activations.as_slice().ok_or("odd style output")?.chunks_exact(STYLES).take(n).map(|r| r.to_vec()).collect();
        let mut artist_rows: Vec<Vec<f32>> = Vec::with_capacity(n);
        for batch in all.chunks(ARTIST_BATCH * size) {
            let real = batch.len() / size;
            let mut full = batch.to_vec();
            full.resize(ARTIST_BATCH * size, 0.0);
            let input: Tensor = tract_ndarray::Array3::from_shape_vec((ARTIST_BATCH, PATCH, BANDS), full).map_err(|e| e.to_string())?.into();
            let out = self.artist.run(tvec!(input.into())).map_err(|e| format!("The artist model stopped: {e}"))?;
            let embeddings = out[0].to_plain_array_view::<f32>().map_err(|e| e.to_string())?;
            let rows = embeddings.as_slice().ok_or("odd artist output")?;
            artist_rows.extend(rows.chunks_exact(ARTIST_DIMS).take(real).map(|r| r.to_vec()));
        }
        if style_rows.len() != n || artist_rows.len() != n {
            return Err("the models gave too few rows".into());
        }
        Ok((style_rows, artist_rows))
    }
}

/// A song made ready to hear: its patches, one after another.
pub struct Patches {
    data: Vec<f32>,
    count: usize,
}

impl Patches {
    /// How many patches it has (0: too short to hear).
    pub fn count(&self) -> usize {
        self.count
    }
}

/// A song (16 kHz mono) made ready to hear: its mel bands, then every
/// `every`th patch (1: all of them; see PATCH_EVERY).
pub fn prepare(audio: &[f32], every: usize) -> Patches {
    let (data, count) = patches(&mel_frames(audio), every);
    Patches { data, count }
}

/// The style model's 400 style names ("Rock---Black Metal"), from its notes
/// (JSON, "classes"), in the order of its activations.
pub fn style_names(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read(path).map_err(|e| format!("The style names didn't load: {e}"))?;
    let json: serde_json::Value = serde_json::from_slice(&text).map_err(|e| format!("The style names didn't load: {e}"))?;
    let names: Vec<String> = json["classes"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default();
    if names.len() != STYLES {
        return Err(format!("The style names didn't load: {} of {STYLES}.", names.len()));
    }
    Ok(names)
}

fn mean(rows: &[Vec<f32>], width: usize) -> Vec<f32> {
    let mut sum = vec![0f64; width];
    for row in rows {
        for (s, v) in sum.iter_mut().zip(row.iter()) {
            *s += *v as f64;
        }
    }
    let n = rows.len().max(1) as f64;
    sum.into_iter().map(|s| (s / n) as f32).collect()
}

/// The file types `hear_file` reads (by extension): FLAC, ALAC or AAC in
/// M4A, MP3, WAV, AIFF, Ogg Vorbis. Not yet: DSD, Opus, WavPack, APE, WMA.
pub fn can_decode(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "flac" | "fla" | "m4a" | "mp4" | "aac" | "alac" | "mp3" | "wav" | "wave" | "aif" | "aiff" | "aifc" | "ogg" | "oga")
}

/// Audio decoded and mixed to mono (the channels averaged, as Essentia's
/// MonoLoader mixes them), handed to `sink` a packet at a time with its
/// sample rate; `sink` returns false to stop there. `ext` hints the format
/// ("mp3", "flac").
fn decode_with(source: Box<dyn symphonia::core::io::MediaSource>, ext: &str, sink: &mut dyn FnMut(&[f32], u32) -> bool) -> Result<(), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let source = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    hint.with_extension(ext);
    let probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("not audio ({e})"))?;
    let mut format = probed.format;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("no audio in it")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("can't decode it ({e})"))?;
    let mut mono: Vec<f32> = Vec::new();
    let mut buffer: Option<(SampleBuffer<f32>, u64)> = None;
    let mut any = false;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // (the end of the file)
            Err(Error::IoError(_)) | Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("couldn't read it ({e})")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // (a damaged frame: skipped, as players do)
            Err(Error::DecodeError(_)) => continue,
            Err(Error::IoError(_)) => break,
            Err(e) => return Err(format!("couldn't decode it ({e})")),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let capacity = decoded.capacity() as u64;
        if buffer.as_ref().is_none_or(|(_, c)| *c < capacity) {
            buffer = Some((SampleBuffer::new(capacity, spec), capacity));
        }
        let (samples, _) = buffer.as_mut().unwrap();
        samples.copy_interleaved_ref(decoded);
        mono.clear();
        if channels == 1 {
            mono.extend_from_slice(samples.samples());
        } else if channels == 2 {
            // (as Essentia's MonoMixer: (left + right) / 2)
            mono.extend(samples.samples().chunks_exact(2).map(|lr| (lr[0] + lr[1]) * 0.5));
        } else {
            mono.extend(samples.samples().chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
        }
        if mono.is_empty() || spec.rate == 0 {
            continue;
        }
        any = true;
        if !sink(&mono, spec.rate) {
            break;
        }
    }
    if !any {
        return Err("no audio in it".into());
    }
    Ok(())
}

/// A preview (MP3 or M4A) as mono samples and its sample rate. `ext` hints
/// the format ("mp3", "m4a").
pub fn decode(bytes: Vec<u8>, ext: &str) -> Result<(Vec<f32>, u32), String> {
    let mut mono: Vec<f32> = Vec::new();
    let mut rate = 0;
    decode_with(Box::new(std::io::Cursor::new(without_id3(bytes))), ext, &mut |chunk, r| {
        rate = r;
        mono.extend_from_slice(chunk);
        true
    })?;
    Ok((mono, rate))
}

/// One of your own files heard whole: decoded (`ext` hints the format, see
/// `can_decode`) and brought to 16 kHz as it goes (its full-rate samples
/// never all held), then SONG_PATCHES patches spread over it; of a long
/// one, its first MOST_SECONDS. Takes a second or two: run it off the async
/// threads.
pub fn hear_file(path: &Path, ext: &str) -> Result<Patches, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("couldn't open it ({e})"))?;
    let most = MOST_SECONDS * RATE as usize;
    let mut audio: Vec<f32> = Vec::new();
    let mut resampler: Option<Resampler> = None;
    decode_with(Box::new(file), ext, &mut |chunk, rate| {
        resampler.get_or_insert_with(|| Resampler::new(rate)).push(chunk, &mut audio);
        audio.len() < most
    })?;
    audio.truncate(most);
    Ok(prepare_spread(&audio, SONG_PATCHES))
}

/// An MP3 without the ID3 tag in front (Deezer's previews start with an
/// empty one, which the format reader stumbles on).
fn without_id3(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() < 10 || &bytes[..3] != b"ID3" {
        return bytes;
    }
    // (the size is "syncsafe": 7 bits a byte; a footer adds 10 more)
    let size = bytes[6..10].iter().fold(0usize, |n, b| (n << 7) | (*b & 0x7f) as usize);
    let footer = if bytes[5] & 0x10 != 0 { 10 } else { 0 };
    let end = (10 + size + footer).min(bytes.len());
    bytes.drain(..end);
    bytes
}

/// Brings mono samples to 16 kHz as the models' makers do in their
/// examples (Essentia's MonoLoader with resampleQuality 4: libsamplerate's
/// linear converter), down to its arithmetic: the ratio worked out in
/// single precision, each output sample a straight line between its two
/// neighbours, a copy of the first sample just before the start. (For two
/// samples or fewer, a sample more than libsamplerate's; nothing that short
/// is heard.)
pub fn resample(input: &[f32], from: u32) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::with_capacity((input.len() as f64 * RATE as f64 / from.max(1) as f64) as usize + 2);
    Resampler::new(from).push(input, &mut out);
    out
}

/// `resample` a piece at a time, for a song as it's decoded: the same
/// samples out, sample for sample, as all of it at once.
pub struct Resampler {
    step: f64,
    /// Where the next output sample sits between its two neighbours (0 to 1).
    index: f64,
    /// The neighbour to its right, counted from the first sample.
    used: usize,
    /// Samples taken in so far, and the last of them.
    seen: usize,
    last: f32,
    started: bool,
    /// Already 16 kHz: passed on as they are.
    same: bool,
}

impl Resampler {
    pub fn new(from: u32) -> Resampler {
        let ratio = (RATE as f32 / from.max(1) as f32) as f64;
        Resampler { step: 1.0 / ratio, index: 0.0, used: 0, seen: 0, last: 0.0, started: false, same: from == RATE || from == 0 }
    }

    /// The next samples in; the 16 kHz samples they make possible onto `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.same {
            out.extend_from_slice(input);
            return;
        }
        if input.is_empty() {
            return;
        }
        let base = self.seen;
        self.seen += input.len();
        if !self.started {
            // Before the first sample: between the copy and the first
            // sample, both the same
            self.started = true;
            while self.index < 1.0 {
                out.push(input[0]);
                self.index += self.step;
            }
            let rem = fmod_one(self.index);
            self.used = (self.index - rem).round_ties_even() as usize;
            self.index = rem;
        }
        let last = self.last;
        let at = |i: usize| if i >= base { input[i - base] } else { last };
        while self.used < self.seen {
            let (a, b) = (at(self.used - 1) as f64, at(self.used) as f64);
            out.push((a + self.index * (b - a)) as f32);
            self.index += self.step;
            let rem = fmod_one(self.index);
            self.used += (self.index - rem).round_ties_even() as usize;
            self.index = rem;
        }
        self.last = input[input.len() - 1];
    }
}

/// libsamplerate's fmod_one: the part after the point, its way (to the
/// nearest whole number, ties to even, as lrint rounds).
fn fmod_one(x: f64) -> f64 {
    let rest = x - x.round_ties_even();
    if rest < 0.0 {
        rest + 1.0
    } else {
        rest
    }
}

/// The 96 mel bands' weights: Slaney's mel scale from 0 to 8 kHz, 98
/// edges evenly spaced on it, each band a triangle over the spectrum's bins
/// (scaled so its area is 1). Each band as (its first bin, its weights).
pub fn mel_bank() -> Vec<(usize, Vec<f32>)> {
    let log_step = 6.4f64.ln() / 27.0;
    let hz_to_mel = |f: f64| if f < 1000.0 { f * 3.0 / 200.0 } else { 15.0 + (f / 1000.0).ln() / log_step };
    let mel_to_hz = |m: f64| if m < 15.0 { m * 200.0 / 3.0 } else { 1000.0 * ((m - 15.0) * log_step).exp() };
    let top = hz_to_mel(RATE as f64 / 2.0);
    let edges: Vec<f64> = (0..BANDS + 2).map(|i| mel_to_hz(top * i as f64 / (BANDS + 1) as f64)).collect();
    let bin_hz = RATE as f64 / FRAME as f64;
    (0..BANDS)
        .map(|b| {
            let (lo, mid, hi) = (edges[b], edges[b + 1], edges[b + 2]);
            let area = 2.0 / (hi - lo);
            let mut first = None;
            let mut weights = Vec::new();
            for j in 0..=FRAME / 2 {
                let f = j as f64 * bin_hz;
                let w = if f > lo && f < mid {
                    (f - lo) / (mid - lo)
                } else if f >= mid && f < hi {
                    (hi - f) / (hi - mid)
                } else {
                    continue;
                };
                first.get_or_insert(j);
                weights.push((w * area) as f32);
            }
            (first.unwrap_or(0), weights)
        })
        .collect()
}

/// What the models read, as Essentia's TensorflowInputMusiCNN makes it,
/// frame for frame: 512-sample frames 256 apart, the first centred on the
/// first sample (zeros before the start and after the end), a Hann window,
/// the power spectrum into the 96 mel bands, then log10(1 + 10000 x).
/// Frames one after another, 96 values each.
pub fn mel_frames(audio: &[f32]) -> Vec<f32> {
    if audio.is_empty() {
        return Vec::new();
    }
    let frames = audio.len().div_ceil(HOP) + 1;
    let mut out = Vec::with_capacity(frames * BANDS);
    Mel::new().frames(audio, 0, frames, &mut out);
    out
}

/// `mel_frames`' workings, for any of a song's frames.
struct Mel {
    window: Vec<f32>,
    bank: Vec<(usize, Vec<f32>)>,
    fft: Arc<dyn rustfft::Fft<f32>>,
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    power: Vec<f32>,
}

impl Mel {
    fn new() -> Mel {
        let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(FRAME);
        let scratch = vec![Complex::new(0f32, 0f32); fft.get_inplace_scratch_len()];
        Mel {
            window: (0..FRAME).map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / (FRAME - 1) as f64).cos()) as f32).collect(),
            bank: mel_bank(),
            fft,
            buf: vec![Complex::new(0f32, 0f32); FRAME],
            scratch,
            power: vec![0f32; FRAME / 2 + 1],
        }
    }

    /// Frames `first` to `first + count` of the audio onto `out`, 96
    /// values each.
    fn frames(&mut self, audio: &[f32], first: usize, count: usize, out: &mut Vec<f32>) {
        for f in first..first + count {
            let start = (f * HOP) as isize - (FRAME / 2) as isize;
            for (i, (slot, w)) in self.buf.iter_mut().zip(&self.window).enumerate() {
                let at = start + i as isize;
                let v = if at >= 0 { audio.get(at as usize).copied().unwrap_or(0.0) } else { 0.0 };
                *slot = Complex::new(v * w, 0.0);
            }
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            for (p, c) in self.power.iter_mut().zip(&self.buf) {
                *p = c.norm_sqr();
            }
            for (first, weights) in &self.bank {
                let energy: f32 = weights.iter().zip(&self.power[*first..]).map(|(w, p)| w * p).sum();
                out.push((1.0 + 10_000.0 * energy).log10());
            }
        }
    }
}

/// A whole song (16 kHz mono) made ready to hear: `n` of its patches spread
/// evenly over it, each in the middle of its stretch of the song (all of
/// them when it has no more), the mel bands worked out for just those.
pub fn prepare_spread(audio: &[f32], n: usize) -> Patches {
    let frames = if audio.is_empty() { 0 } else { audio.len().div_ceil(HOP) + 1 };
    if frames < PATCH || n == 0 {
        return Patches { data: Vec::new(), count: 0 };
    }
    let count = (frames - PATCH) / PATCH_HOP + 1;
    let picked: Vec<usize> = if count <= n { (0..count).collect() } else { (0..n).map(|i| (2 * i + 1) * count / (2 * n)).collect() };
    let mut mel = Mel::new();
    let mut data = Vec::with_capacity(picked.len() * PATCH * BANDS);
    for p in &picked {
        mel.frames(audio, p * PATCH_HOP, PATCH, &mut data);
    }
    Patches { data, count: picked.len() }
}

/// The patches the models take: 128 frames each, 62 apart, a last one
/// that would run past the end left out; of those every `every`th (1: all);
/// at most 64, spread over the song when there are more. The patches one
/// after another, and how many.
pub fn patches(frames: &[f32], every: usize) -> (Vec<f32>, usize) {
    let total = frames.len() / BANDS;
    if total < PATCH {
        return (Vec::new(), 0);
    }
    let count = (total - PATCH) / PATCH_HOP + 1;
    let all: Vec<usize> = (0..count).step_by(every.max(1)).collect();
    let picked: Vec<usize> = if all.len() <= MOST_PATCHES {
        all
    } else {
        (0..MOST_PATCHES).map(|i| all[i * (all.len() - 1) / (MOST_PATCHES - 1)]).collect()
    };
    let mut out = Vec::with_capacity(picked.len() * PATCH * BANDS);
    for p in &picked {
        let at = p * PATCH_HOP * BANDS;
        out.extend_from_slice(&frames[at..at + PATCH * BANDS]);
    }
    (out, picked.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_bands_cover_the_spectrum() {
        let bank = mel_bank();
        assert_eq!(bank.len(), BANDS);
        // (each band a triangle of area 1: its weights times the bin width
        // add up to about 1; up to 7% off where a band spans only two or
        // three bins, around 1.3 kHz, as in Essentia's)
        for (b, (first, weights)) in bank.iter().enumerate() {
            assert!(!weights.is_empty(), "band {b} is empty");
            assert!(*first + weights.len() <= FRAME / 2 + 1);
            let area: f32 = weights.iter().sum::<f32>() * (RATE as f32 / FRAME as f32);
            let off = if b >= 50 { 0.05 } else { 0.1 };
            assert!((area - 1.0).abs() < off, "band {b}: {area}");
        }
        // (the first band holds bin 1 only; the top band ends below 8 kHz)
        assert_eq!(bank[0].0, 1);
        assert_eq!(bank[0].1.len(), 1);
        assert_eq!(bank[95].0 + bank[95].1.len(), 256);
    }

    #[test]
    fn frames_and_patches_line_up() {
        // (30 s of a 440 Hz tone: 1876 frames, as Essentia makes them, and
        // 29 patches)
        let audio: Vec<f32> = (0..479_818).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / RATE as f32).sin() * 0.5).collect();
        let frames = mel_frames(&audio);
        assert_eq!(frames.len(), 1876 * BANDS);
        // (the tone lands in the band around 440 Hz: the 14th, mel 6.6)
        let middle = &frames[900 * BANDS..901 * BANDS];
        let loudest = (0..BANDS).max_by(|a, b| middle[*a].total_cmp(&middle[*b])).unwrap();
        assert!((13..=14).contains(&loudest), "{loudest}");
        let (all, n) = patches(&frames, 1);
        assert_eq!(n, 29);
        assert_eq!(all.len(), 29 * PATCH * BANDS);
        // (patch 1 starts at frame 62)
        assert_eq!(all[PATCH * BANDS], frames[62 * BANDS]);
        // (every other one: 15, the second at frame 124; four previews to
        // one of the artist model's batches)
        let (half, n) = patches(&frames, PATCH_EVERY);
        assert_eq!(n, 15);
        assert_eq!(half[PATCH * BANDS], frames[124 * BANDS]);
        assert_eq!(ARTIST_BATCH / n, 4);
    }

    #[test]
    fn linear_resampling_like_libsamplerate() {
        // (44.1 kHz to 16 kHz: output n sits at input n * 2.75625 - 1, the
        // first ones on a copy of the first sample)
        let input: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let out = resample(&input, 44_100);
        assert_eq!(out.len(), (1000.0f64 * 16_000.0 / 44_100.0).ceil() as usize);
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 1.75625).abs() < 1e-3, "{}", out[1]);
        assert!((out[100] - (100.0 * 2.75625 - 1.0)).abs() < 1e-2, "{}", out[100]);
        // (16 kHz stays as it is)
        assert_eq!(resample(&input, 16_000), input);
    }

    /// The whole-signal version this was first written as (libsamplerate's
    /// linear converter, all at once), to hold the piece-at-a-time one to.
    fn resample_at_once(input: &[f32], from: u32) -> Vec<f32> {
        let ratio = (RATE as f32 / from as f32) as f64;
        let step = 1.0 / ratio;
        let total = input.len() as f64;
        let mut out: Vec<f32> = Vec::new();
        let mut index = 0.0f64;
        while index < 1.0 {
            if 1.0 + index >= total {
                return out;
            }
            out.push(input[0]);
            index += step;
        }
        let rem = fmod_one(index);
        let mut used = (index - rem).round_ties_even() as usize;
        index = rem;
        while (used as f64) + index < total {
            let (a, b) = (input[used - 1] as f64, input[used] as f64);
            out.push((a + index * (b - a)) as f32);
            index += step;
            let rem = fmod_one(index);
            used += (index - rem).round_ties_even() as usize;
            index = rem;
        }
        out
    }

    #[test]
    fn resampling_a_piece_at_a_time_is_the_same() {
        // (a wobbly signal, in pieces of odd sizes, at the common rates)
        let input: Vec<f32> = (0..200_003).map(|i| ((i as f32) * 0.013).sin() * 0.7 + ((i * 7919 % 1000) as f32 / 5000.0)).collect();
        for from in [44_100, 48_000, 88_200, 96_000, 192_000, 22_050, 11_025, 8_000] {
            let whole = resample_at_once(&input, from);
            assert_eq!(resample(&input, from), whole, "{from} Hz at once");
            let mut pieces = Resampler::new(from);
            let mut out = Vec::new();
            let mut at = 0;
            let mut size = 1;
            while at < input.len() {
                let end = (at + size).min(input.len());
                pieces.push(&input[at..end], &mut out);
                at = end;
                size = (size * 7 + 3) % 4099 + 1;
            }
            assert_eq!(out, whole, "{from} Hz in pieces");
        }
    }

    #[test]
    fn a_whole_song_is_heard_at_patches_spread_over_it() {
        // (5 minutes: 280 patches, of which 32, one in the middle of each
        // stretch, the same mel bands as the whole song's)
        let audio: Vec<f32> = (0..300 * RATE as usize).map(|i| ((i as f32) * 0.05).sin() * (0.2 + (i % 48_000) as f32 / 96_000.0)).collect();
        let frames = mel_frames(&audio);
        let total = frames.len() / BANDS;
        let count = (total - PATCH) / PATCH_HOP + 1;
        assert_eq!(count, 301);
        let spread = prepare_spread(&audio, SONG_PATCHES);
        assert_eq!(spread.count(), SONG_PATCHES);
        assert_eq!(spread.data.len(), SONG_PATCHES * PATCH * BANDS);
        for (i, patch) in spread.data.chunks(PATCH * BANDS).enumerate() {
            let p = (2 * i + 1) * count / (2 * SONG_PATCHES);
            let at = p * PATCH_HOP * BANDS;
            assert_eq!(patch, &frames[at..at + PATCH * BANDS], "patch {i} (the song's {p})");
        }
        // (the first near the start, the last near the end)
        assert_eq!((2 * 0 + 1) * count / (2 * SONG_PATCHES), 4);
        assert_eq!((2 * 31 + 1) * count / (2 * SONG_PATCHES), 296);
        // (a short one: all its patches)
        assert_eq!(prepare_spread(&audio[..20 * RATE as usize], SONG_PATCHES).count(), 19);
        assert_eq!(prepare_spread(&audio[..1000], SONG_PATCHES).count(), 0);
    }

    #[test]
    fn hears_a_file_as_it_decodes() {
        // (a 45-second WAV at 44.1 kHz, stereo: heard from the file, a
        // piece at a time, the same as decoded and brought to 16 kHz whole)
        let rate = 44_100u32;
        let frames = 45 * rate as usize;
        let mut wav: Vec<u8> = Vec::new();
        let data_len = (frames * 4) as u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let left = ((2.0 * std::f32::consts::PI * 220.0 * t).sin() * 12_000.0) as i16;
            let right = ((2.0 * std::f32::consts::PI * 330.0 * t).sin() * 9_000.0) as i16;
            wav.extend_from_slice(&left.to_le_bytes());
            wav.extend_from_slice(&right.to_le_bytes());
        }
        let dir = std::env::temp_dir().join(format!("toasted-audio-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.wav");
        std::fs::write(&path, &wav).unwrap();
        let heard = hear_file(&path, "wav").unwrap();
        let (mono, from) = decode(wav, "wav").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(from, rate);
        assert_eq!(mono.len(), frames);
        let whole = prepare_spread(&resample(&mono, from), SONG_PATCHES);
        assert_eq!(heard.count(), SONG_PATCHES);
        assert_eq!(heard.data, whole.data);
        assert!(can_decode("FLAC") && can_decode("m4a") && !can_decode("dsf"));
    }
}
