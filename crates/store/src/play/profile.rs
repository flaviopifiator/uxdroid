//! The device Google Play is told about: a generic one of uxdroid's own, with this machine's ABI,
//! the SDK level the platform reports, and the density and languages the installer chooses splits
//! for, so the splits Play serves are the ones the installer keeps.
//!
//! The names describe Android's emulator, the generic device Google's own SDK publishes, because Play
//! serves apps only to devices it can classify; nothing identifies the user or this machine.

use sha2::{Digest, Sha256};

use super::proto::Writer;

/// What Play's build of the store app reports, as recent store apps do.
const VENDING_VERSION: &str = "84772030";
const VENDING_VERSION_NAME: &str = "47.7.20-31 [0] [PR] 816318457";
const GSF_VERSION: &str = "250932022";
const BUILD_ID: &str = "BP22.250325.006";
const RELEASE: &str = "16";
/// OpenGL ES 3.2, as the platform's EGL reports it.
const GL_VERSION: u64 = 0x0003_0002;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// `x86_64` or `arm64-v8a`.
    pub abi: String,
    pub sdk: u32,
    pub density: u32,
    /// The user's languages, preferred first, as lowercase codes such as `en`.
    pub languages: Vec<String>,
}

struct Names {
    device: &'static str,
    product: &'static str,
    platforms: &'static [&'static str],
    fingerprint: &'static str,
}

impl Profile {
    fn names(&self) -> Names {
        if self.abi == "arm64-v8a" {
            Names {
                device: "emu64a",
                product: "sdk_gphone64_arm64",
                platforms: &["arm64-v8a"],
                fingerprint: "google/sdk_gphone64_arm64/emu64a:16/BP22.250325.006/13344233:user/release-keys",
            }
        } else {
            Names {
                device: "emu64xa",
                product: "sdk_gphone64_x86_64",
                platforms: &["x86_64"],
                fingerprint: "google/sdk_gphone64_x86_64/emu64xa:16/BP22.250325.006/13344233:user/release-keys",
            }
        }
    }

    /// The languages, each once, with English last when it is not among them, since most apps
    /// ship it in their base.
    fn locales(&self) -> Vec<String> {
        let mut locales: Vec<String> = Vec::new();
        for language in self.languages.iter().chain([&"en".to_string()]) {
            if !language.is_empty() && !locales.contains(language) {
                locales.push(language.clone());
            }
        }
        locales
    }

    const FEATURES: &'static [&'static str] = &[
        "android.hardware.audio.output",
        "android.hardware.faketouch",
        "android.hardware.screen.landscape",
        "android.hardware.screen.portrait",
        "android.hardware.touchscreen",
        "android.hardware.touchscreen.multitouch",
        "android.hardware.wifi",
        "android.software.webview",
    ];

    const LIBRARIES: &'static [&'static str] = &[
        "android.test.base",
        "android.test.mock",
        "android.test.runner",
        "org.apache.http.legacy",
    ];

    /// The SHA-256 of the check-in, which holds the whole device configuration: a sign-in is kept
    /// for it, and another profile signs in again.
    pub(crate) fn digest(&self) -> String {
        super::proto::hex(&Sha256::digest(self.checkin()))
    }

    /// The user agent of Play's store app on this device.
    pub(crate) fn user_agent(&self) -> String {
        let names = self.names();
        format!(
            "Android-Finsky/{VENDING_VERSION_NAME} (api=3,versionCode={VENDING_VERSION},sdk={},device={},hardware=ranchu,product={},platformVersionRelease={RELEASE},model={},buildId={BUILD_ID},isWideScreen=0,supportedAbis={})",
            self.sdk,
            names.device,
            names.product,
            names.product,
            names.platforms.join(";")
        )
    }

    /// The user agent of Google's sign-in on this device.
    pub(crate) fn auth_user_agent(&self) -> String {
        format!("GoogleAuth/1.4 ({} {BUILD_ID})", self.names().device)
    }

    /// The languages as Play's headers name them, such as `en_US`.
    pub(crate) fn user_languages(&self) -> String {
        self.locales().join(",")
    }

    pub(crate) fn gsf_version(&self) -> &'static str {
        GSF_VERSION
    }

    /// The device configuration message of Android's check-in and of `uploadDeviceConfig`.
    pub(crate) fn configuration(&self, writer: &mut Writer) {
        writer
            .uint(1, 3) // touch screen: finger
            .uint(2, 1) // keyboard: none
            .uint(3, 1) // navigation: none
            .uint(4, 2) // screen layout: normal
            .boolean(5, false)
            .boolean(6, false)
            .uint(7, u64::from(self.density))
            .uint(8, GL_VERSION);
        for library in Self::LIBRARIES {
            writer.string(9, library);
        }
        for feature in Self::FEATURES {
            writer.string(10, feature);
        }
        for platform in self.names().platforms {
            writer.string(11, platform);
        }
        writer.uint(12, 1080).uint(13, 1920);
        for locale in self.locales() {
            writer.string(14, &locale);
        }
    }

    /// Android's check-in request for a new device with no account.
    pub(crate) fn checkin(&self) -> Vec<u8> {
        let names = self.names();
        let gsf: u64 = GSF_VERSION.parse().expect("a number");
        Writer::new()
            .uint(2, 0)
            .message(4, |checkin| {
                checkin
                    .message(1, |build| {
                        build
                            .string(1, names.fingerprint)
                            .string(2, "ranchu")
                            .string(3, "google")
                            .string(4, "unknown")
                            .string(5, "unknown")
                            .string(6, "android-google")
                            .uint(7, 0)
                            .uint(8, gsf)
                            .string(9, names.device)
                            .uint(10, u64::from(self.sdk))
                            .string(11, names.product)
                            .string(12, "Google")
                            .string(13, names.product)
                            .boolean(14, false);
                    })
                    .uint(2, 0)
                    .string(6, "310260")
                    .string(7, "310260")
                    .string(8, "mobile-notroaming")
                    .uint(9, 0);
            })
            .string(6, &self.locales()[0])
            .string(12, "UTC")
            .uint(14, 3)
            .message(18, |configuration| self.configuration(configuration))
            .uint(20, 0)
            .finish()
    }
}
