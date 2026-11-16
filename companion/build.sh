#!/bin/bash
# Builds the companion app's APK without Gradle: aapt2, javac, d8, zipalign,
# apksigner from the Android SDK (build-tools 35, platform 34).
#
#   ANDROID_SDK   the SDK folder (default: $ANDROID_HOME, or $HOME/android/sdk)
#   KEYSTORE      the signing key (default: debug.keystore here, password "android",
#                 made on the first build; never commit a keystore, the release key
#                 stays private)
#   KS_PASS       its password (default: android)
#
# Out: build/toasted-companion.apk
set -e
cd "$(dirname "$0")"
SDK=${ANDROID_SDK:-${ANDROID_HOME:-$HOME/android/sdk}}
BT=$SDK/build-tools/35.0.0
JAR=$SDK/platforms/android-34/android.jar
KEYSTORE=${KEYSTORE:-debug.keystore}
KS_PASS=${KS_PASS:-android}
if [ ! -f "$KEYSTORE" ]; then
  keytool -genkeypair -keystore "$KEYSTORE" -storepass "$KS_PASS" -keypass "$KS_PASS" -alias debug \
    -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Debug" >/dev/null
fi
rm -rf build
mkdir -p build/classes build/dex build/res
"$BT/aapt2" compile --dir res -o build/res/
"$BT/aapt2" link -o build/unsigned.apk --manifest AndroidManifest.xml -I "$JAR" build/res/*.flat
javac -source 8 -target 8 -cp "$JAR" -d build/classes $(find src -name "*.java")
"$BT/d8" --lib "$JAR" --min-api 26 --output build/dex $(find build/classes -name "*.class")
(cd build/dex && zip -q ../unsigned.apk classes.dex)
"$BT/zipalign" -f 4 build/unsigned.apk build/aligned.apk
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass "pass:$KS_PASS" --out build/toasted-companion.apk build/aligned.apk
ls -la build/toasted-companion.apk
