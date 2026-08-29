//! Saving a video, from the page's offer to the file on disk.
//!
//! The conversation is always the same: the page says what it can give, a list
//! is put in front of the user, one row is chosen, and the bytes are pulled on a
//! thread of its own while others do the same. None of that depends on which
//! window is drawing it, so it lives here — the settled window and the one
//! window both drive this, and "what saving means" has one definition.

use crate::download::youtube::{self, AudioWish, Offer};
use crate::download::save;
use crate::engine::Shared;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// What a running download has to report.
enum Step {
    Progress(u64, u64),
    Done,
    Failed(String),
}

/// One download among possibly several, and where it has got to.
pub struct Job {
    /// What the page calls it when it asks for it to be stopped.
    pub id: u64,
    pub title: String,
    /// The file this is going to end up as — folder and name together. Two
    /// downloads with the same one would be two threads writing one file.
    key: String,
    rx: std::sync::mpsc::Receiver<Step>,
    pub done: u64,
    pub total: u64,
    pub state: State,
    /// The finished line: that it saved, or why it did not.
    pub note: String,
    cancel: Arc<AtomicBool>,
}

#[derive(PartialEq, Clone, Copy)]
pub enum State {
    Running,
    Done,
    Failed,
}

impl Job {
    /// How far along, between nothing and all of it.
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
    }
}

/// The marker itag the music-only row carries, so a click on it is told apart
/// from a click on a real video row. No real format uses it.
pub const MUSIC_ITAG: u32 = u32::MAX;

/// A direct progressive download (a plain media URL), for non-YouTube sites.
pub const DIRECT_ITAG: u32 = u32::MAX - 1;

/// An HLS stream, downloaded as segments and joined into one file.
pub const HLS_ITAG: u32 = u32::MAX - 2;

/// Whether the page said to go ahead despite the warning.
pub fn forced(payload: &str) -> bool {
    payload.contains("\"force\"")
}

/// The itag in `{"choose":401}`, without pulling in a parser for one number.
pub fn chosen(payload: &str) -> Option<u32> {
    let at = payload.find("\"choose\"")?;
    let rest = &payload[at + 8..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Whether a file of this name is already on the shelf, wherever it has been
/// filed since.
///
/// By the name it would be saved under, not by the address it came from: what
/// the user recognises is the title, and a video re-uploaded under a new address
/// is the same thing arriving twice.
fn already_saved(kind: crate::library::Kind, title: &str) -> bool {
    let name = save::safe_name(title);
    crate::library::items(kind).iter().any(|item| item.title == name)
}

/// Bytes, as a person reads them.
pub fn human(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    match bytes {
        0 => "크기 미상".into(),
        b if b >= GB => format!("{:.1} GB", b as f64 / GB as f64),
        b if b >= MB => format!("{} MB", b / MB),
        b => format!("{} KB", (b / 1024).max(1)),
    }
}

/// Where saved videos go: the user's own Videos folder, in a folder of ours —
/// beside their other videos, the way music lands beside their music.
pub fn videos_folder() -> std::path::PathBuf {
    home().join("Videos").join("Shard")
}

/// Where music-only downloads land: the user's own Music folder, under Shard.
pub fn music_folder() -> std::path::PathBuf {
    home().join("Music").join("Shard")
}

fn home() -> std::path::PathBuf {
    std::env::var("USERPROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

/// One row per resolution, carrying the smallest encoding of it.
///
/// The full list runs to sixteen entries because each resolution is offered in
/// three codecs, and reading it means comparing the same number three times to
/// reach an answer that is always the same: take the small one.
pub fn rows_for(offer: &Offer, wish: &AudioWish) -> Vec<(u32, String, String)> {
    let Some(audio) = offer.best_audio(wish) else { return Vec::new() };
    let mut seen: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for video in offer.video_tracks() {
        if seen.contains(&video.quality) {
            continue;
        }
        seen.push(video.quality.clone());
        let total = video.size() + audio.size();
        // Marked when the number is ours rather than the page's, so a figure
        // that turns out to be off by a few per cent does not read as a lie.
        let size = if video.size_is_exact() { human(total) } else { format!("약 {}", human(total)) };
        rows.push((
            video.itag,
            video.quality.clone(),
            format!(
                "{} · {} + {} {}k",
                size,
                video.codec(),
                audio.codec(),
                audio.bitrate / 1000
            ),
        ));
    }
    rows
}

/// The music-only row, shown at the top of the list.
pub fn music_row(offer: &Offer, wish: &AudioWish) -> Option<(u32, String, String)> {
    let audio = offer.best_audio(wish)?;
    Some((
        MUSIC_ITAG,
        "음악만 저장".into(),
        format!(
            "{} · {} {}k{}",
            human(audio.size()),
            audio.codec(),
            audio.bitrate / 1000,
            if audio.audio_name.is_empty() {
                String::new()
            } else {
                format!(" · {}", audio.audio_name)
            }
        ),
    ))
}

/// What came of draining the downloads: what to say, and whether a shelf changed.
pub struct Drained {
    /// One line per download that ended, and whether it ended badly.
    pub finished: Vec<(String, bool)>,
    /// Set when something new is on a shelf, so the library re-reads it.
    pub saved: bool,
}

/// Everything in flight, and the offer the list was built from.
pub struct Downloads {
    shared: Arc<Shared>,
    /// What the page last offered, kept so a click can be matched to a format.
    offer: Option<Offer>,
    /// The HLS renditions of the last offer, if it was one, so a chosen quality
    /// row maps back to the playlist to download.
    variants: Vec<crate::download::hls::Variant>,
    pub list: Vec<Job>,
    next_id: u64,
}

/// The first itag an HLS quality row carries; the rest count up from it. Above
/// every real YouTube itag and below the two markers, so the three never clash.
const HLS_VARIANT_BASE: u32 = 90_000;

impl Downloads {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared, offer: None, variants: Vec::new(), list: Vec::new(), next_id: 1 }
    }

    /// What the settings say about sound, in the form the chooser wants.
    ///
    /// `portable` is set for the music-only row: a file saved on its own has to
    /// play on a phone rather than only in this program, so it takes the
    /// compatible codec. A video's own soundtrack does not, and picks the
    /// smaller one.
    fn wish(&self, portable: bool) -> AudioWish {
        let cfg = self.shared.config.read();
        AudioWish {
            language: cfg.download.audio_language.clone(),
            quality: cfg.download.audio,
            portable,
        }
    }

    /// Turn what the page reported into a list it can show.
    ///
    /// Returns the script to run on the page — either the list, or a line
    /// saying why there is none.
    pub fn qualities(&mut self, payload: &str) -> String {
        let offer = match Offer::parse(payload) {
            Ok(offer) => offer,
            Err(_) => return youtube::say_script("영상 정보를 읽지 못했습니다.", true),
        };
        // Not YouTube, but the page was seen fetching a media file or an HLS
        // playlist. When it is HLS, its renditions are listed so a quality can be
        // picked the way the phone does; the master is fetched here to read them.
        if offer.template().is_none() && (!offer.media.is_empty() || !offer.hls.is_empty()) {
            let mut rows = Vec::new();
            self.variants = Vec::new();
            if !offer.hls.is_empty() {
                // Try the first playlist, then every other one the page fetched,
                // until one reads as a master with qualities in it. Sites do not
                // agree on which request is the master.
                let mut variants = save::hls_variants(&offer.hls, &offer.referer);
                if variants.is_empty() {
                    for url in offer.hls_list.split('\n').filter(|u| !u.trim().is_empty()) {
                        if url == offer.hls {
                            continue;
                        }
                        let found = save::hls_variants(url, &offer.referer);
                        if !found.is_empty() {
                            variants = found;
                            break;
                        }
                    }
                }
                if variants.is_empty() {
                    // A media playlist, or a master that would not read: one row,
                    // the stream taken whole.
                    rows.push((HLS_ITAG, "최고 화질".to_string(), "HLS 스트림".to_string()));
                } else {
                    for (index, variant) in variants.iter().enumerate() {
                        rows.push((
                            HLS_VARIANT_BASE + index as u32,
                            variant.label(),
                            "HLS 스트림".to_string(),
                        ));
                    }
                    self.variants = variants;
                }
            }
            // The direct file only when there is no HLS: on an HLS site the
            // captured progressive URL is usually a short preview, not the video.
            if !offer.media.is_empty() && offer.hls.is_empty() {
                rows.push((DIRECT_ITAG, "원본 파일".to_string(), "직접 다운로드".to_string()));
            }
            if rows.is_empty() {
                return youtube::say_script("받을 수 있는 형식을 찾지 못했습니다.", true);
            }
            let script = youtube::list_script(&rows);
            self.offer = Some(offer);
            return script;
        }
        if offer.template().is_none() {
            return youtube::say_script(
                if offer.formats.is_empty() {
                    "이 페이지에서 영상 정보를 읽지 못했습니다."
                } else {
                    "영상을 잠시 재생한 뒤 다시 눌러 주세요."
                },
                true,
            );
        }

        // The video rows, and a music-only row at the top — the same shape the
        // phone shows, so there is no separate "include video" setting to find.
        let mut rows = rows_for(&offer, &self.wish(false));
        if let Some(music) = music_row(&offer, &self.wish(true)) {
            rows.insert(0, music);
        }
        if rows.is_empty() {
            // Counted after the filters this code applies, not before: the first
            // version of this message reported what the page listed and so said
            // twenty audio formats while the chooser had none.
            let video = offer.video_tracks().len();
            let audio = offer.formats.iter().filter(|f| f.is_audio()).count();
            return youtube::say_script(
                &format!(
                    "받을 수 있는 화질이 없습니다.\n(형식 {} · 쓸 수 있는 영상 {} · 음성 {})",
                    offer.formats.len(),
                    video,
                    audio
                ),
                true,
            );
        }
        let script = youtube::list_script(&rows);
        self.offer = Some(offer);
        script
    }

    /// Start fetching the chosen format on a thread of its own.
    ///
    /// Returns the line to put on the page. It says so and goes: the download
    /// runs on, and how far along it is belongs in the Shard window rather than
    /// on top of what is being watched.
    pub fn begin(&mut self, itag: u32, anyway: bool) -> String {
        let Some(offer) = self.offer.clone() else {
            return youtube::say_script("받을 것을 찾지 못했습니다.", true);
        };
        // The non-YouTube rows: a direct fetch, or an HLS join at some quality.
        // None has a SABR template, so they are answered here before the template
        // is asked for below.
        if itag == DIRECT_ITAG {
            return self.begin_media(&offer, offer.media.clone(), false, anyway);
        }
        if itag == HLS_ITAG {
            return self.begin_media(&offer, offer.hls.clone(), true, anyway);
        }
        // Bounded to the real variant count: the music marker itag is u32::MAX,
        // which is also >= the base, and without this bound a music download was
        // taken for an HLS quality and refused with "고른 화질을 찾지 못했습니다".
        if itag >= HLS_VARIANT_BASE && (itag - HLS_VARIANT_BASE) < self.variants.len() as u32 {
            let index = (itag - HLS_VARIANT_BASE) as usize;
            let Some(variant) = self.variants.get(index) else {
                return youtube::say_script("고른 화질을 찾지 못했습니다.", true);
            };
            return self.begin_media(&offer, variant.url.clone(), true, anyway);
        }
        let Some(template) = offer.template() else {
            return youtube::say_script("받을 것을 찾지 못했습니다.", true);
        };
        // The row decides it: the music row carries a marker itag, every other
        // row a real video one.
        let audio_only = itag == MUSIC_ITAG;
        let wish = self.wish(audio_only);
        let Some(audio) = offer.best_audio(&wish) else {
            return youtube::say_script("음성을 찾지 못했습니다.", true);
        };
        let Some(video) = (if audio_only {
            offer.video_tracks().into_iter().last()
        } else {
            offer.formats.iter().find(|f| f.itag == itag)
        }) else {
            return youtube::say_script("고른 화질을 찾지 못했습니다.", true);
        };
        // Naming the wanted format as already playing costs the stream's opening
        // bytes, so anything else is named instead.
        let decoy = offer
            .video_tracks()
            .into_iter()
            .find(|f| f.itag != itag)
            .map(|f| f.track())
            .unwrap_or_else(|| audio.track());

        let job = save::Job {
            template,
            video: video.track(),
            audio: audio.track(),
            decoy,
            title: offer.title.clone(),
            // Music of its own goes to a Music folder, not among the videos, so
            // a folder of songs is a folder of songs.
            into: if audio_only { music_folder() } else { videos_folder() },
            cover: offer.thumb.clone(),
            audio_only,
            mp4: false,   // desktop plays through the WebView, which opens MKV/WebM too
            // MP3 only makes sense for a music-only save, and only when the user
            // asked for it; the flag is read where the file is written.
            music_mp3: audio_only && self.shared.config.read().download.music_mp3,
        };
        // Music-only never fetches the video (the "video" here is only a small decoy
        // named as already-playing), so counting its bytes in the total made the bar
        // stall at audio/(video+audio) — ~65% on a short song — and then jump to done.
        // The audio's own size is the whole of an audio-only download.
        let expected = if audio_only { job.audio.bytes } else { job.video.bytes + job.audio.bytes };
        let title = offer.title.clone();

        // The same video twice at once writes one file from two threads, and
        // what lands is neither of them. Pressing a row twice — while the page
        // is still there and the panel has not closed yet — is an easy thing to
        // do, so it is answered rather than obeyed.
        let key = format!("{}|{}", job.into.display(), title);
        if self
            .list
            .iter()
            .any(|other| other.state == State::Running && other.key == key)
        {
            return youtube::say_script("이미 받고 있습니다.", true);
        }

        // Already on the shelf, from some other day. Said rather than refused —
        // a file may be worth having again, cut short the first time or saved
        // before something in here was fixed — and the way past it is on the
        // panel, so it is one press rather than finding the row again.
        let shelf =
            if audio_only { crate::library::Kind::Music } else { crate::library::Kind::Video };
        if !anyway && already_saved(shelf, &title) {
            return youtube::again_script("이미 보관함에 있습니다.", itag);
        }

        let (tx, rx) = std::sync::mpsc::channel::<Step>();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let mut report = |p: crate::download::pull::Progress| {
                let _ = tx.send(Step::Progress(p.video + p.audio, expected));
            };
            let outcome = save::run(&job, &mut report, &|| stop.load(Ordering::Relaxed));
            let _ = tx.send(match outcome {
                Ok(_) => Step::Done,
                Err(e) => Step::Failed(format!("{e:#}")),
            });
        });

        let id = self.next_id;
        self.next_id += 1;
        self.list.push(Job {
            id,
            title,
            key,
            rx,
            done: 0,
            total: expected,
            state: State::Running,
            note: String::new(),
            cancel,
        });
        youtube::flash_script("받는 중")
    }

    /// Start a direct or HLS download — the non-YouTube path.
    ///
    /// No template, no separate audio track: the file (or the joined stream) is
    /// one thing, so this is much thinner than [`begin`]. It reuses the same
    /// job list, progress channel and cancel flag so the panel treats it no
    /// differently once it is running.
    fn begin_media(&mut self, offer: &Offer, url: String, hls: bool, anyway: bool) -> String {
        let title = if offer.title.trim().is_empty() {
            "video".to_string()
        } else {
            offer.title.clone()
        };
        if url.is_empty() {
            return youtube::say_script("받을 주소를 찾지 못했습니다.", true);
        }
        let into = videos_folder();
        let key = format!("{}|{}", into.display(), title);
        if self.list.iter().any(|o| o.state == State::Running && o.key == key) {
            return youtube::say_script("이미 받고 있습니다.", true);
        }
        if !anyway && already_saved(crate::library::Kind::Video, &title) {
            return youtube::again_script("이미 보관함에 있습니다.", if hls { HLS_ITAG } else { DIRECT_ITAG });
        }

        let referer = offer.referer.clone();
        let title_for_job = title.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Step>();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::spawn(move || {
            let mut report = |done: u64, total: u64| {
                let _ = tx.send(Step::Progress(done, total));
            };
            let outcome = if hls {
                save::run_hls(&url, &referer, "", "", "", &into, &title_for_job, &mut report, &|| {
                    stop.load(Ordering::Relaxed)
                })
            } else {
                save::run_direct(&url, &referer, "", "", "", &into, &title_for_job, &mut report, &|| {
                    stop.load(Ordering::Relaxed)
                })
            };
            let _ = tx.send(match outcome {
                Ok(_) => Step::Done,
                Err(e) => Step::Failed(format!("{e:#}")),
            });
        });

        let id = self.next_id;
        self.next_id += 1;
        self.list.push(Job {
            id,
            title,
            key,
            rx,
            done: 0,
            total: 0,
            state: State::Running,
            note: String::new(),
            cancel,
        });
        youtube::flash_script("받는 중")
    }

    /// Stop one, by the number the page knows it as.
    pub fn cancel(&self, id: u64) {
        if let Some(job) = self.list.iter().find(|j| j.id == id) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Take in what the running downloads have said, and let the finished ones go.
    pub fn drain(&mut self) -> Drained {
        let mut saved = false;
        for job in &mut self.list {
            while let Ok(step) = job.rx.try_recv() {
                match step {
                    Step::Progress(done, total) => {
                        job.done = done;
                        if total > 0 {
                            job.total = total;
                        }
                    }
                    Step::Done => {
                        job.state = State::Done;
                        job.note = "저장했습니다".into();
                        // There is a new file on a shelf; the library reads it
                        // again rather than polling to notice what this program
                        // has just put there.
                        saved = true;
                    }
                    Step::Failed(why) => {
                        job.state = State::Failed;
                        job.note = format!("실패: {why}");
                    }
                }
            }
        }
        // A success says so and goes; a failure stays until it has been read.
        let finished: Vec<(String, bool)> = self
            .list
            .iter()
            .filter(|j| j.state != State::Running)
            .map(|j| (j.note.clone(), j.state == State::Failed))
            .collect();
        self.list.retain(|j| j.state == State::Running);
        Drained { finished, saved }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_that_was_pressed_is_read_out_of_the_pages_message() {
        assert_eq!(chosen(r#"{"choose":401}"#), Some(401));
        assert_eq!(chosen(r#"{"choose": 22 }"#), Some(22));
        // The music row carries a marker the rest of the flow tells apart.
        assert_eq!(chosen(&format!(r#"{{"choose":{MUSIC_ITAG}}}"#)), Some(MUSIC_ITAG));
        assert_eq!(chosen(r#"{"ask":true}"#), None);
    }

    #[test]
    fn sizes_read_the_way_a_person_says_them() {
        assert_eq!(human(0), "크기 미상");
        assert_eq!(human(1 << 20), "1 MB");
        assert_eq!(human(3 << 30), "3.0 GB");
    }

    #[test]
    fn the_two_shelves_are_the_users_own_media_folders() {
        assert!(videos_folder().ends_with(std::path::Path::new("Videos").join("Shard")));
        assert!(music_folder().ends_with(std::path::Path::new("Music").join("Shard")));
    }
}
