// C ABI shim over zsign (github.com/zhlynn/zsign) so the Rust resign engine can
// call zsign's bundle signer via FFI. Why zsign and not apple-codesign: for iOS 26
// free-dev resigning of an app with old-minos frameworks (MobileVLCKit iOS 9.0),
// apple-codesign force-injects SHA-1 primary CDs into nested frameworks and iOS
// rejects them (0xe8008001) — a byte-level nested-signing limit we cannot override
// with settings. zsign signs the whole bundle (main + frameworks) in one pass with
// SHA-256-only primary CDs by default (zsign.cpp bSHA256Only=true), which is exactly
// what SideStore ships on iOS 26. See memory ios-zsign-ondevice-pivot.
//
// We compile zsign's src/*.cpp (minus zsign.cpp, its CLI main which also does
// fork/exec install we don't use) + this shim, and link OpenSSL from the app's
// OpenSSL.xcframework (already fetched by ios-app.yml). The engine writes cert/key/
// profile/entitlements to temp files and passes their paths here.

#include "common.h"
#include "bundle.h"
#include "openssl.h"

#include <string>
#include <vector>

// Returns 0 on success, non-zero on failure. Signs the already-extracted .app
// FOLDER in place (main executable + nested frameworks).
//
// prov_file / entitle_file may be "" (empty) — zsign treats empty as "not provided".
extern "C" int zsign_sign_folder(
    const char *app_folder,
    const char *cert_file,
    const char *key_file,
    const char *prov_file,
    const char *entitle_file,
    const char *bundle_id) {
    if (app_folder == nullptr || cert_file == nullptr || key_file == nullptr) {
        return 2;
    }
    const char *prov = (prov_file != nullptr) ? prov_file : "";
    const char *ent = (entitle_file != nullptr) ? entitle_file : "";
    const char *bid = (bundle_id != nullptr) ? bundle_id : "";

    ZSignAsset zsa;
    // Init(cert, pkey, prov, entitle, password, bAdhoc, bSHA256Only, bSingleBinary)
    // bAdhoc=false: real signature. bSHA256Only=true: zsign's default, the iOS 26
    // answer. bSingleBinary=false: this is a bundle (multiple Mach-Os), not one exe.
    if (!zsa.Init(cert_file, key_file, prov, ent, "", false, true, false)) {
        return 3;
    }

    ZBundle bundle;
    // SignFolder(asset, folder, bundleId, version, displayName, injectDylibs,
    //            removeDylibs, bForce, bWeakInject, bEnableCache, bRemoveProvision)
    bool ok = bundle.SignFolder(
        &zsa, app_folder, bid, "", "",
        std::vector<std::string>(), std::vector<std::string>(),
        /*bForce=*/true, /*bWeakInject=*/false, /*bEnableCache=*/false,
        /*bRemoveProvision=*/false);
    return ok ? 0 : 1;
}
