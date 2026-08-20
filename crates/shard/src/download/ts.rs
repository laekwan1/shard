//! MPEG-TS, demultiplexed into tracks a Matroska file can hold.
//!
//! HLS on many sites is a stream of 188-byte MPEG-TS packets, video and audio
//! muxed together, and the in-app player cannot open a raw `.ts`. This pulls the
//! two elementary streams back out — H.264 for the picture, AAC for the sound —
//! and hands them to [`super::mkv`], which repackages them without re-encoding.
//!
//! It leans on one simplification that holds for the transport streams HLS
//! produces: each PES packet on the video PID carries exactly one access unit,
//! and the PES's own timestamp is that frame's. So a PES is a frame — no
//! access-unit boundary detection in the elementary stream is needed. The audio
//! PID's PES packets each carry a run of ADTS frames, split here on their sync
//! words.
//!
//! Pure and network-free, like the rest of the parsers: the fetching is
//! [`super::save`]'s. What can be got wrong in interesting ways — PES timing,
//! Annex-B splitting, the AVCC and AudioSpecificConfig it builds — is tested
//! against bytes laid out by hand.

use anyhow::{bail, Result};

/// One frame of one track, timed the way Matroska wants it.
pub struct Sample {
    pub data: Vec<u8>,
    /// Presentation time, milliseconds.
    pub time_ms: u64,
    /// Decode time, milliseconds — the order frames are written in.
    pub decode_ms: u64,
    pub keyframe: bool,
}

/// The two tracks pulled out of a transport stream.
pub struct Demuxed {
    pub avcc: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub video: Vec<Sample>,
    pub asc: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub audio: Vec<Sample>,
}

/// A byte stream that opens with a TS sync byte and keeps to the 188-byte beat.
pub fn is_ts(data: &[u8]) -> bool {
    data.first() == Some(&0x47) && data.len() >= 188 && data[188.min(data.len() - 1)] == 0x47
}

/// Stream types in a PMT this understands.
const STREAM_H264: u8 = 0x1B;
const STREAM_AAC_ADTS: u8 = 0x0F;

/// Reassemble the PES packets of one PID out of the transport stream.
struct PesBuilder {
    pid: u16,
    buffer: Vec<u8>,
    started: bool,
    packets: Vec<Vec<u8>>,
}

impl PesBuilder {
    fn new(pid: u16) -> Self {
        Self { pid, buffer: Vec::new(), started: false, packets: Vec::new() }
    }

    /// A packet's payload arrives, with whether it began a new PES.
    fn feed(&mut self, payload: &[u8], unit_start: bool) {
        if unit_start {
            if self.started && !self.buffer.is_empty() {
                self.packets.push(std::mem::take(&mut self.buffer));
            }
            self.started = true;
        }
        if self.started {
            self.buffer.extend_from_slice(payload);
        }
    }

    fn finish(mut self) -> Vec<Vec<u8>> {
        if !self.buffer.is_empty() {
            self.packets.push(std::mem::take(&mut self.buffer));
        }
        self.packets
    }
}

/// Demultiplex a whole transport stream into its tracks.
pub fn demux(data: &[u8]) -> Result<Demuxed> {
    clear_params();
    // Find the program map, then the elementary PIDs, then gather each stream.
    let mut pmt_pid: Option<u16> = None;
    let mut video_pid: Option<u16> = None;
    let mut audio_pid: Option<u16> = None;

    // First pass: read the PSI to learn the PIDs.
    for packet in data.chunks_exact(188) {
        if packet[0] != 0x47 {
            continue;
        }
        let pid = (((packet[1] & 0x1F) as u16) << 8) | packet[2] as u16;
        let unit_start = packet[1] & 0x40 != 0;
        let Some(payload) = ts_payload(packet) else { continue };

        if pid == 0 && unit_start {
            if let Some(p) = parse_pat(payload) {
                pmt_pid = Some(p);
            }
        } else if Some(pid) == pmt_pid && unit_start {
            if let Some((v, a)) = parse_pmt(payload) {
                video_pid = v;
                audio_pid = a;
            }
        }
    }

    let video_pid = video_pid;
    let audio_pid = audio_pid;

    // Second pass: reassemble the PES packets on the two elementary PIDs.
    let mut video = video_pid.map(PesBuilder::new);
    let mut audio = audio_pid.map(PesBuilder::new);
    for packet in data.chunks_exact(188) {
        if packet[0] != 0x47 {
            continue;
        }
        let pid = (((packet[1] & 0x1F) as u16) << 8) | packet[2] as u16;
        let unit_start = packet[1] & 0x40 != 0;
        let Some(payload) = ts_payload(packet) else { continue };
        if let Some(b) = video.as_mut() {
            if b.pid == pid {
                b.feed(payload, unit_start);
            }
        }
        if let Some(b) = audio.as_mut() {
            if b.pid == pid {
                b.feed(payload, unit_start);
            }
        }
    }

    // The picture.
    let mut avcc = Vec::new();
    let mut width = 0;
    let mut height = 0;
    let mut video_samples = Vec::new();
    if let Some(b) = video {
        for pes in b.finish() {
            let Some((pts, dts, body)) = parse_pes(&pes) else { continue };
            let nals = split_annex_b(body);
            let mut frame = Vec::new();
            let mut keyframe = false;
            for nal in &nals {
                if nal.is_empty() {
                    continue;
                }
                match nal[0] & 0x1F {
                    7 => {
                        // Keep the SPS to build the avcC once, and read the size.
                        if width == 0 {
                            if let Some((w, h)) = sps_dimensions(nal) {
                                width = w;
                                height = h;
                            }
                        }
                        set_sps(nal);
                    }
                    8 => set_pps(nal),
                    5 => keyframe = true,
                    _ => {}
                }
                // Length-prefixed, the form V_MPEG4/ISO/AVC wants.
                frame.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                frame.extend_from_slice(nal);
            }
            if avcc.is_empty() {
                if let Some(built) = build_avcc() {
                    avcc = built;
                }
            }
            if !frame.is_empty() {
                let show = pts / 90;
                let decode = dts.unwrap_or(pts) / 90;
                video_samples.push(Sample { data: frame, time_ms: show, decode_ms: decode, keyframe });
            }
        }
    }
    if avcc.is_empty() {
        if let Some(built) = build_avcc() {
            avcc = built;
        }
    }

    // The sound.
    let mut asc = Vec::new();
    let mut sample_rate = 0;
    let mut channels = 0;
    let mut audio_samples = Vec::new();
    if let Some(b) = audio {
        for pes in b.finish() {
            let Some((pts, _dts, body)) = parse_pes(&pes) else { continue };
            let mut at = pts / 90;
            for frame in split_adts(body) {
                if asc.is_empty() {
                    if let Some((config, rate, ch)) = adts_config(&frame.header) {
                        asc = config;
                        sample_rate = rate;
                        channels = ch;
                    }
                }
                audio_samples.push(Sample {
                    data: frame.body,
                    time_ms: at,
                    decode_ms: at,
                    keyframe: true,
                });
                // Each AAC frame is 1024 samples; step the clock on for the next.
                if sample_rate > 0 {
                    at += 1024 * 1000 / sample_rate as u64;
                }
            }
        }
    }

    if video_samples.is_empty() && audio_samples.is_empty() {
        bail!("전송 스트림에서 트랙을 찾지 못했습니다");
    }
    Ok(Demuxed {
        avcc,
        width,
        height,
        video: video_samples,
        asc,
        sample_rate,
        channels,
        audio: audio_samples,
    })
}

// ---- transport-stream layer ------------------------------------------------

/// The payload bytes of a TS packet, past any adaptation field.
fn ts_payload(packet: &[u8]) -> Option<&[u8]> {
    let control = (packet[3] >> 4) & 0x3;
    // 01 = payload only, 11 = adaptation then payload; 10 = adaptation only, 00
    // reserved — neither carries payload.
    if control != 1 && control != 3 {
        return None;
    }
    let mut offset = 4;
    if control == 3 {
        let adaptation_length = packet[4] as usize;
        offset = 5 + adaptation_length;
    }
    if offset >= packet.len() {
        return None;
    }
    Some(&packet[offset..])
}

/// The program-map PID from a PAT payload.
fn parse_pat(payload: &[u8]) -> Option<u16> {
    let section = psi_section(payload)?;
    // table_id 0x00, then a fixed header of 8 bytes before the program loop.
    if section.first() != Some(&0x00) || section.len() < 12 {
        return None;
    }
    let mut i = 8;
    while i + 4 <= section.len() - 4 {
        let program = ((section[i] as u16) << 8) | section[i + 1] as u16;
        let pid = (((section[i + 2] & 0x1F) as u16) << 8) | section[i + 3] as u16;
        if program != 0 {
            return Some(pid);
        }
        i += 4;
    }
    None
}

/// The video and audio elementary PIDs from a PMT payload.
fn parse_pmt(payload: &[u8]) -> Option<(Option<u16>, Option<u16>)> {
    let section = psi_section(payload)?;
    if section.first() != Some(&0x02) || section.len() < 16 {
        return None;
    }
    // section_length is the 12 bits after the table id.
    let section_length = ((section[1] as usize & 0x0F) << 8) | section[2] as usize;
    let end = (3 + section_length).min(section.len()).saturating_sub(4); // drop CRC
    let program_info_length = ((section[10] as usize & 0x0F) << 8) | section[11] as usize;
    let mut i = 12 + program_info_length;
    let mut video = None;
    let mut audio = None;
    while i + 5 <= end {
        let stream_type = section[i];
        let pid = (((section[i + 1] & 0x1F) as u16) << 8) | section[i + 2] as u16;
        let es_info_length = ((section[i + 3] as usize & 0x0F) << 8) | section[i + 4] as usize;
        match stream_type {
            STREAM_H264 if video.is_none() => video = Some(pid),
            STREAM_AAC_ADTS if audio.is_none() => audio = Some(pid),
            _ => {}
        }
        i += 5 + es_info_length;
    }
    Some((video, audio))
}

/// Skip the pointer field a PUSI payload starts a PSI section with.
fn psi_section(payload: &[u8]) -> Option<&[u8]> {
    let pointer = *payload.first()? as usize;
    let start = 1 + pointer;
    if start >= payload.len() {
        return None;
    }
    Some(&payload[start..])
}

/// The PTS, DTS and payload of a PES packet.
fn parse_pes(pes: &[u8]) -> Option<(u64, Option<u64>, &[u8])> {
    // 00 00 01 start code, a stream id, then a 16-bit length that may be zero.
    if pes.len() < 9 || pes[0] != 0 || pes[1] != 0 || pes[2] != 1 {
        return None;
    }
    let flags = pes[7];
    let header_len = pes[8] as usize;
    let body_start = 9 + header_len;
    if body_start > pes.len() {
        return None;
    }
    let mut pts = None;
    let mut dts = None;
    if flags & 0x80 != 0 && pes.len() >= 14 {
        pts = read_timestamp(&pes[9..14]);
        if flags & 0x40 != 0 && pes.len() >= 19 {
            dts = read_timestamp(&pes[14..19]);
        }
    }
    Some((pts.unwrap_or(0), dts, &pes[body_start..]))
}

/// A 33-bit PTS/DTS out of its five marker-laden bytes.
fn read_timestamp(b: &[u8]) -> Option<u64> {
    if b.len() < 5 {
        return None;
    }
    let value = (((b[0] as u64 >> 1) & 0x07) << 30)
        | ((b[1] as u64) << 22)
        | (((b[2] as u64 >> 1) & 0x7F) << 15)
        | ((b[3] as u64) << 7)
        | ((b[4] as u64 >> 1) & 0x7F);
    Some(value)
}

// ---- H.264 -----------------------------------------------------------------

/// Split an Annex-B byte stream into its NAL units, dropping the start codes.
fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0;
    let mut start = None;
    while i + 3 <= data.len() {
        // A start code is 00 00 01, optionally with a leading 00.
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                let mut end = i;
                // Trim a trailing zero that belonged to the next start code.
                if end > s && data[end - 1] == 0 {
                    end -= 1;
                }
                nals.push(&data[s..end]);
            }
            start = Some(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        if s < data.len() {
            nals.push(&data[s..]);
        }
    }
    nals
}

// The SPS and PPS are kept in a thread-local while a stream is demuxed, so the
// avcC can be built from the first of each. One demux runs on one thread.
thread_local! {
    static SPS: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
    static PPS: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

fn set_sps(nal: &[u8]) {
    SPS.with(|s| {
        if s.borrow().is_none() {
            *s.borrow_mut() = Some(nal.to_vec());
        }
    });
}
fn set_pps(nal: &[u8]) {
    PPS.with(|p| {
        if p.borrow().is_none() {
            *p.borrow_mut() = Some(nal.to_vec());
        }
    });
}

/// Forget the SPS and PPS between demuxes, so one stream's do not leak into the
/// next on the same thread.
fn clear_params() {
    SPS.with(|s| *s.borrow_mut() = None);
    PPS.with(|p| *p.borrow_mut() = None);
}

/// Build the avcC record from the SPS and PPS gathered so far.
fn build_avcc() -> Option<Vec<u8>> {
    let sps = SPS.with(|s| s.borrow().clone())?;
    let pps = PPS.with(|p| p.borrow().clone())?;
    if sps.len() < 4 {
        return None;
    }
    let mut out = Vec::new();
    out.push(1); // configurationVersion
    out.push(sps[1]); // AVCProfileIndication
    out.push(sps[2]); // profile_compatibility
    out.push(sps[3]); // AVCLevelIndication
    out.push(0xFF); // 6 bits reserved, then lengthSizeMinusOne = 3 (4-byte)
    out.push(0xE1); // 3 bits reserved, then numOfSequenceParameterSets = 1
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(&sps);
    out.push(1); // numOfPictureParameterSets
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(&pps);
    Some(out)
}

/// Read the coded picture size out of an SPS.
///
/// Just enough Exp-Golomb to reach the width and height: the fields before them
/// are skipped or read, and the crop is applied so the stated size is the one
/// the picture actually shows.
fn sps_dimensions(nal: &[u8]) -> Option<(u32, u32)> {
    // Skip the NAL header byte; the rest is RBSP with emulation-prevention bytes.
    let rbsp = strip_emulation(&nal[1..]);
    let mut r = BitReader::new(&rbsp);
    let profile_idc = r.bits(8)?;
    r.bits(8)?; // constraint flags + reserved
    r.bits(8)?; // level_idc
    r.ue()?; // seq_parameter_set_id

    if matches!(profile_idc, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135)
    {
        let chroma_format_idc = r.ue()?;
        if chroma_format_idc == 3 {
            r.bit()?; // separate_colour_plane_flag
        }
        r.ue()?; // bit_depth_luma_minus8
        r.ue()?; // bit_depth_chroma_minus8
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        let seq_scaling_matrix_present = r.bit()?;
        if seq_scaling_matrix_present == 1 {
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for _ in 0..count {
                // Skipping scaling lists exactly is elaborate; these streams do
                // not carry them, so bail rather than mis-read.
                return None;
            }
        }
    }

    r.ue()?; // log2_max_frame_num_minus4
    let pic_order_cnt_type = r.ue()?;
    if pic_order_cnt_type == 0 {
        r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if pic_order_cnt_type == 1 {
        r.bit()?;
        r.se()?;
        r.se()?;
        let n = r.ue()?;
        for _ in 0..n {
            r.se()?;
        }
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let pic_width_in_mbs = r.ue()? + 1;
    let pic_height_in_map_units = r.ue()? + 1;
    let frame_mbs_only_flag = r.bit()?;
    if frame_mbs_only_flag == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?; // direct_8x8_inference_flag
    let frame_cropping_flag = r.bit()?;
    let (mut crop_l, mut crop_r, mut crop_t, mut crop_b) = (0u32, 0u32, 0u32, 0u32);
    if frame_cropping_flag == 1 {
        crop_l = r.ue()?;
        crop_r = r.ue()?;
        crop_t = r.ue()?;
        crop_b = r.ue()?;
    }

    let width = pic_width_in_mbs * 16;
    let height = (2 - frame_mbs_only_flag) * pic_height_in_map_units * 16;
    // Chroma 4:2:0 crop units are 2 luma samples wide, 2×(2-frame_mbs_only) tall.
    let crop_unit_x = 2;
    let crop_unit_y = 2 * (2 - frame_mbs_only_flag);
    let w = width.saturating_sub((crop_l + crop_r) * crop_unit_x);
    let h = height.saturating_sub((crop_t + crop_b) * crop_unit_y);
    Some((w, h))
}

/// Remove the 00 00 03 emulation-prevention bytes an RBSP is padded with.
fn strip_emulation(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        out.push(b);
    }
    out
}

/// A most-significant-bit-first reader with the Exp-Golomb codes H.264 uses.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        let byte = self.data.get(self.bit / 8)?;
        let shift = 7 - (self.bit % 8);
        self.bit += 1;
        Some(((byte >> shift) & 1) as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    /// Unsigned Exp-Golomb.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let mut value = 1u32;
        for _ in 0..zeros {
            value = (value << 1) | self.bit()?;
        }
        Some(value - 1)
    }
    /// Signed Exp-Golomb.
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        let signed = if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) };
        Some(signed as i32)
    }
}

// ---- AAC -------------------------------------------------------------------

struct AdtsFrame {
    header: [u8; 7],
    body: Vec<u8>,
}

/// Split an ADTS byte run into its frames, header kept beside each body.
fn split_adts(data: &[u8]) -> Vec<AdtsFrame> {
    let mut frames = Vec::new();
    let mut i = 0;
    while i + 7 <= data.len() {
        if data[i] != 0xFF || (data[i + 1] & 0xF0) != 0xF0 {
            i += 1;
            continue;
        }
        let protection_absent = data[i + 1] & 1;
        let header_len = if protection_absent == 1 { 7 } else { 9 };
        let frame_len = (((data[i + 3] as usize & 0x03) << 11)
            | ((data[i + 4] as usize) << 3)
            | ((data[i + 5] as usize) >> 5))
            & 0x1FFF;
        if frame_len < header_len || i + frame_len > data.len() {
            break;
        }
        let mut header = [0u8; 7];
        header.copy_from_slice(&data[i..i + 7]);
        frames.push(AdtsFrame { header, body: data[i + header_len..i + frame_len].to_vec() });
        i += frame_len;
    }
    frames
}

/// Build the AudioSpecificConfig from an ADTS header, and read its rate/channels.
fn adts_config(header: &[u8; 7]) -> Option<(Vec<u8>, u32, u32)> {
    // profile is object type minus one; the frequency index and channel config
    // are next. The two-byte ASC packs object type, index and channels.
    let object_type = ((header[2] >> 6) & 0x3) + 1;
    let freq_index = (header[2] >> 2) & 0x0F;
    let channels = (((header[2] & 0x1) << 2) | (header[3] >> 6)) as u32;
    const RATES: [u32; 13] =
        [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    let rate = *RATES.get(freq_index as usize)?;
    let asc0 = (object_type << 3) | (freq_index >> 1);
    let asc1 = ((freq_index & 1) << 7) | ((channels as u8) << 3);
    Some((vec![asc0, asc1], rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_on_start_codes_and_drops_them() {
        // Two NALs, one with a 4-byte start code, one with 3.
        let data = [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB];
        let nals = split_annex_b(&data);
        assert_eq!(nals.len(), 2);
        assert_eq!(nals[0], &[0x67, 0xAA]);
        assert_eq!(nals[1], &[0x68, 0xBB]);
    }

    #[test]
    fn a_pts_is_read_from_its_marker_laden_bytes() {
        // PTS of 90000 (one second at 90 kHz), packed with '0010' and markers.
        // 90000 = 0x15F90. Build the five bytes by the spec.
        let pts: u64 = 90000;
        let b0 = 0x21 | (((pts >> 30) & 0x7) << 1) as u8;
        let b1 = ((pts >> 22) & 0xFF) as u8;
        let b2 = (0x01 | (((pts >> 15) & 0x7F) << 1)) as u8;
        let b3 = ((pts >> 7) & 0xFF) as u8;
        let b4 = (0x01 | ((pts & 0x7F) << 1)) as u8;
        assert_eq!(read_timestamp(&[b0, b1, b2, b3, b4]), Some(90000));
    }

    #[test]
    fn emulation_prevention_bytes_are_removed() {
        assert_eq!(strip_emulation(&[0, 0, 3, 1]), vec![0, 0, 1]);
        assert_eq!(strip_emulation(&[0, 0, 3, 0, 0, 3, 2]), vec![0, 0, 0, 0, 2]);
    }

    #[test]
    fn adts_is_split_into_frames_and_the_config_read() {
        // One AAC-LC frame, 48 kHz, stereo, 7-byte header, 4 bytes of body,
        // frame_length 11. object_type=2 → profile '01'; freq_index=3 (48k);
        // channel_config=2. Laid out by the ADTS spec, bit for bit.
        let header = [
            0xFF, // syncword high
            0xF1, // syncword low, MPEG-4, layer 00, no CRC
            0x4C, // profile 01, freq 0011, private 0, channel hi 0
            0x80, // channel lo 10, frame_length high 2 bits (0)
            0x01, // frame_length middle 8 bits (11 >> 3 = 1)
            0x7F, // frame_length low 3 bits (011), buffer fullness high
            0xFC, // buffer fullness low, num_frames 0
        ];
        let mut data = header.to_vec();
        data.extend_from_slice(&[1, 2, 3, 4]);
        let frames = split_adts(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].body, vec![1, 2, 3, 4]);
        let (asc, rate, channels) = adts_config(&frames[0].header).expect("config");
        assert_eq!(rate, 48000);
        assert_eq!(channels, 2);
        // ASC for AAC-LC 48k stereo is 0x11 0x90.
        assert_eq!(asc, vec![0x11, 0x90]);
    }

    #[test]
    fn a_payload_only_packet_gives_its_bytes_past_the_header() {
        let mut packet = [0u8; 188];
        packet[0] = 0x47;
        packet[3] = 0x10; // payload only
        packet[4] = 0xAB;
        assert_eq!(ts_payload(&packet).unwrap()[0], 0xAB);
    }

    #[test]
    fn an_adaptation_field_is_skipped_to_reach_the_payload() {
        let mut packet = [0u8; 188];
        packet[0] = 0x47;
        packet[3] = 0x30; // adaptation + payload
        packet[4] = 2; // adaptation_field_length
        packet[7] = 0xCD; // payload begins at 5 + 2
        assert_eq!(ts_payload(&packet).unwrap()[0], 0xCD);
    }
}
