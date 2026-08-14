//! Saving video that a page is playing.
//!
//! Shard's engine makes a blocked site reachable; this is the other half of
//! what people actually do with it. The phone app has had it for a while and
//! the desktop has not, for a structural reason: on Android the browser *is*
//! the app, so the page and the downloader share a process. Here the engine
//! works on packets and has no page at all.
//!
//! What a page provides that packets cannot is context — the address a media
//! request needs, the headers a CDN insists on, and for YouTube a whole
//! captured request. So the desktop grows a small browser window of its own
//! rather than trying to reconstruct any of that from the wire.
//!
//! [`sabr`] is the YouTube half and is deliberately free of both the browser
//! and the network: it turns a captured request into the next one to send, and
//! a reply into bytes with positions. That makes the part most likely to break
//! the part that is cheapest to test.

#[cfg(all(feature = "desktop", windows))]
pub mod browser;
pub mod ebml;
pub mod mkv;
pub mod mp4;
pub mod pull;
pub mod sabr;
#[cfg(feature = "desktop")]
pub mod save;
pub mod webm;
pub mod youtube;
