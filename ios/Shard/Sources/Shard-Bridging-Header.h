// Exposes the Rust C ABI to Swift.
#import "ShardCore.h"
// Routes libVLC's decoded audio to our AVAudioEngine output (clean over Bluetooth).
#import "VLCAudioBridge.h"
// Captures libVLC's decoded VP9 frames so we present them in sync with our audio.
#import "VLCVideoBridge.h"
