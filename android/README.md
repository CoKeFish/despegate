# despegate for Android

The same idea as on Windows, on a phone: recurring rules, blocks started on the spot, forced breaks and daily allowances per app. While a block is active or about to start, nothing that loosens despegate is accepted. The way out is uninstalling, which first shows you your own reasons.

It is a separate app, written in Kotlin; it shares the look and the behaviour of the Windows version, not its code.

## What it does to the phone

There are two ways for despegate to enforce anything, and it uses the stronger one it has been given.

**Watching**, through an accessibility service. Nothing on the phone has to be given up for it:

- **Lock the screen**: whenever an app that is not allowed comes to the front, despegate puts its lock page back. Calls always work, and so do the apps on the allowed list.
- **Block apps**: a blocked app is sent back to the home screen with a word on until when. A video it left floating over the screen is dragged away.
- **Hold its ground**: while a block is active or about to start, the settings screens that name despegate (its accessibility switch, its app info, the uninstall dialog) are closed too.

It makes leaving hard and uncomfortable, not impossible: outside those moments the service can be switched off from the phone's settings.

**Owning the phone**, as its device owner: the highest level of control Android gives an app without rooting.

- **Lock the screen**: the system itself fixes the phone to the lock page and the allowed apps. Home leads back to it, other apps cannot be opened, notifications are hidden.
- **Block apps**: the apps a rule names are suspended for as long as it is active; so is an app whose daily allowance is spent.
- **Close the side doors**: safe mode and extra users are off, and the clock cannot be changed while a block is active or about to start.

What no app can prevent: switching the phone off, and wiping it from recovery mode.

## Building

Needs the Android SDK (platform 34) and a JDK 17 or 21. From this directory:

```
gradlew assembleDebug
gradlew assembleRelease
```

- **debug** is the trial build. A computer can take it off the phone again (`adb shell dpm remove-active-admin io.github.cokefish.despegate/.platform.Admin`), which Android only allows for test-only apps. Install it with `adb install -t`.
- **release** has no such back door: its one way out is the app's own uninstall. Both are signed with this machine's debug key, and later updates must be signed with the same key.

## Setting it up

### Watching

```
adb install -t app/build/outputs/apk/debug/app-debug.apk
adb shell appops set io.github.cokefish.despegate ACCESS_RESTRICTED_SETTINGS allow
```

The second line is needed because Android does not let an app installed from outside a store be given an accessibility service; it can also be done from the app's info screen ("Allow restricted settings"). Then open despegate, go to Settings and follow it to the accessibility settings to switch the service on. On a Samsung, also take the app out of battery saving, or the phone will switch the service off when it puts the app to sleep; the app offers to.

### Owning the phone

Android only lets a device owner be set on a phone with **no accounts on it**, from a computer:

1. On the phone, enable developer options and USB debugging.
2. Remove every account (Settings, Accounts). On a Samsung that includes the Samsung account; Secure Folder, Dual Messenger and work profiles must be gone too, because they count as extra users.
3. Install the app and hand the phone over:

   ```
   adb install -t app/build/outputs/apk/debug/app-debug.apk
   adb shell dpm set-device-owner io.github.cokefish.despegate/.platform.Admin
   adb shell appops set io.github.cokefish.despegate GET_USAGE_STATS allow
   ```

   The last line lets despegate see which app is in front, which the daily allowances need; it can also be granted from the app.
4. Add the accounts back.

Try the debug build first, and put the apps you cannot do without on the allowed list before the first lock.

## Leaving

Settings, at the bottom: *Uninstall*. The same link is on the lock screen. It shows your reasons, asks you to type a phrase, lifts every block, stops being the device owner and asks Android to uninstall the app.

## How it is put together

- `core/` is the engine, with nothing of Android in it: the config and its rules, the clocks behind breaks and allowances, what counts as loosening, and the decision of what to enforce at a given instant. It is covered by unit tests (`gradlew testDebugUnitTest`).
- `platform/` acts on the phone: `Enforcer` looks at the clock every two seconds while the screen is on and makes the phone match; `Device` is the device owner's control and `Guard` the service that keeps it alive; `Watch` is the accessibility service that does the job on a phone despegate does not own; `Phone` is what the phone can tell about its use.
- `ui/` holds the two screens. Each is a page (`assets/index.html`, `assets/lock.html`) inside a web view, talking to the app through `window.ipc.postMessage` and hearing back through `window.__reply`, as on Windows.
