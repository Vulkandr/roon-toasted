# Toasted Companion (Android)

The phone side of Roon: Toasted's Living Queue. It reads Roon ARC on the phone
and lets Toasted show and control it as a zone ("ARC on <phone>").

- Needs **Notification access** (the only way Android shares another app's
  media controls) and runs a quiet foreground service, so battery saving doesn't
  put it to sleep.
- Finds Toasted on the same Wi-Fi (UDP broadcast on port 58422) or takes an
  address, and pairs with the six-digit code on Toasted's Living Queue page.
- Reports ARC's song, state and position, and takes play, pause, next, previous
  and seek back from Toasted, sent through ARC's Android Auto service.
- Toasted's side of the protocol is `src-tauri/src/companion.rs`.

Build with `build.sh` (Android SDK build-tools 35 and platform 34, no Gradle).
Not released yet.
