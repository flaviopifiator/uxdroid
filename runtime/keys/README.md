# Test signing key

`test.pk8` (PKCS #8, RSA 2048) and `test.x509.pem` sign the sample APKs that `runtime/framework/build_system.sh` builds, because `uxdroid install` installs only signed APKs, as Android does. The key is public on purpose: it proves nothing about who built an APK, and must never sign anything outside these samples.

Certificate SHA-256: `8b5915e28dbd2963c5b2089137ab713af81f3f973b913373473608d3d5258ac7`.
