# despegate

> *Despégate* (Spanish): "unglue yourself".

An intrusive blocker that forces you to stop what you are doing when it is time to leave the computer.

## Why

Normal reminders are easy to dismiss when you are deep into a game or any other absorbing task: you click the notification away, keep going, and end up late. despegate is for people who need something that actually interrupts them — blocks that cannot be ignored or silently dismissed.

## Status

The Windows desktop version works. It is young; expect rough edges. A mobile companion is still only an idea.

## What it does

- **Blocks on a schedule.** A rule covers a time window on chosen days, e.g. every night from 23:00 to 07:00.
- **Blocks right now.** `despegate now --for 45m --lock` starts a block that cannot be cancelled.
- **Forces breaks.** After a stretch of continuous use the screen locks for a while. Only time at the keyboard or mouse counts, and stepping away on your own for as long as the break lasts counts as taking it.
- **Limits programs per day.** A program can be given a daily allowance, counted only while its window is in front. When it is spent the program is closed until the next day.
- **Locks the screen.** A lock covers every monitor, stays on top, swallows Alt+Tab, the Windows key and Alt+F4, and closes Task Manager. It shows the time left and your own reasons for installing despegate.
- **Closes programs.** A block can name executables (`steam.exe`, ...). They are closed while the block is active, and closed again if you reopen them.
- **Warns you first.** A banner appears a few minutes before a block, a break, or the end of an allowance.
- **Resists you.** Once installed it runs as a Windows service under the SYSTEM account. It refuses stop requests, an ordinary user cannot end it, and Windows starts it again if an administrator does. Its files cannot be edited behind its back. While a block is active or about to start, nothing that loosens it is accepted.
- **Speaks your language.** English and Spanish so far; it follows Windows unless told otherwise.

## The two ways out

despegate is meant to be hard to switch off in the heat of the moment, not to trap you.

- **Uninstall.** `despegate uninstall` (or *Settings → Apps*) always works for an administrator. It first shows the reasons you wrote and asks you to confirm by typing a phrase.
- **Emergency pause.** The lock screen shows a random text. Typing it exactly (80 characters by default) pauses everything for 5 minutes — enough for a real emergency, or to uninstall. Set `emergency-chars` to `0` to remove this exit; then a mistaken lock rule can only be waited out.

## Install

Requires Windows 10/11 and a [Rust toolchain](https://rustup.rs).

```
cargo build --release
target\release\despegate.exe install
```

`install` asks for administrator rights. It copies the two executables to `C:\Program Files\despegate`, adds that folder to `PATH`, and registers the service. Open a new terminal afterwards.

## Use

```
despegate reasons set "Sleep eight hours. Be on time for the people waiting for me."

# Lock the screen every night; the window ends the next morning.
despegate rule add sleep --from 23:00 --to 07:00 --lock

# Close games during working hours on weekdays.
despegate rule add focus --from 09:00 --to 18:00 --days weekdays --app steam.exe,hearthstone.exe

# Step away right now for 45 minutes.
despegate now --for 45m --lock

# A 10 minute break after every 50 minutes of use.
despegate break set --every 50m --for 10m

# One hour of Hearthstone a day.
despegate allowance set hearthstone.exe 60m

despegate status
```

`--days` takes `all`, `weekdays`, `weekends`, a range like `mon-fri`, or a list like `mon,wed,sat`. A rule can combine `--lock` and `--app`. Every command explains itself with `--help`.

### What can be changed, and when

Anything that makes despegate stricter is accepted at any time: a new rule (even one that is active immediately), shorter work periods, a smaller allowance.

Anything that loosens it is refused while a block or break is active or about to start. In addition, a program's allowance cannot be raised or removed on a day the program has already been used.

### Settings

`despegate set <key> <value>`:

| Key | Default | Meaning |
| --- | --- | --- |
| `lead-minutes` | 30 | How long before a block nothing can be loosened any more |
| `warn-minutes` | 5 | How long before a block the warning banner appears |
| `emergency-chars` | 80 | Length of the text to type for an emergency pause; `0` disables it |
| `emergency-minutes` | 5 | Length of the emergency pause |

### Language

`despegate language` lists the languages; `despegate language es` chooses one and `despegate language auto` goes back to following Windows.

To add a language, copy `locales/en.toml` to `locales/<code>.toml`, translate the texts (keeping the `{placeholders}`), and list the file in `SOURCES` in `src/i18n.rs`. `cargo test` fails if a key or placeholder is missing. The messages of the argument parser itself (for example an invalid time) are not translated.

## Making it stick

As long as your account is an administrator, you can undo despegate from an elevated prompt. The serious setup is to stop being one: create a separate administrator account, keep its password somewhere inconvenient (on paper, in another room, with someone you trust), and make your everyday account a standard user. despegate keeps working exactly the same, and removing it then takes that password.

## How it is built

Two executables share one library:

- `despegate.exe` — the CLI. It sends each request to the daemon over a named pipe; the daemon decides whether to accept it.
- `despegated.exe` — the daemon and its agent.
  - The **daemon** is the service. It holds the configuration and the usage counters, decides what is blocked, and closes programs. It lives in the services session, where it can show nothing and sees no input.
  - The **agent** is started by the daemon inside your session. It draws the lock screen and the banner, and reports idle time and the program in front. If it dies the daemon starts another; if it keeps dying during a lock, the daemon sends the session to the Windows sign-in screen.

State lives in `C:\ProgramData\despegate`: `config.toml`, `state.toml` (usage counters, so that restarting the daemon resets nothing) and `despegate.log`. Everyone can read it; only the daemon writes it.

## Limits

despegate raises the cost of giving in; it does not make it impossible.

- An administrator can get around it: disabling the service from an elevated prompt, changing the system clock, or booting into Safe Mode.
- Ctrl+Alt+Del cannot be intercepted by any program, so signing out or shutting down from there always works — which is, after all, a way of stepping away.
- Programs are matched by executable name only; a renamed copy is a different program.
- Use is measured through keyboard and mouse. Watching a video without touching anything, or playing with a gamepad, looks like being away.
- Allowances reset at midnight.
- Only the session at the physical screen is policed, not remote desktop sessions.

## If something goes wrong

To remove despegate by hand, from an elevated prompt:

```
type nul > C:\ProgramData\despegate\stop
sc delete despegate
taskkill /f /im despegated.exe
rmdir /s /q "C:\Program Files\despegate"
rmdir /s /q C:\ProgramData\despegate
```

The `stop` file makes the daemon exit on its own within a second. If a lock screen is in the way, start in Safe Mode, where the service does not run.

## Ideas

Not built, and not promised:

- Blocking websites (hosts file plus browser policies).
- Starting a block early at a natural pause during the warning, instead of at the last second.
- Matching programs by folder as well as by name.
- Counting how often a blocked program was opened.
- Prebuilt releases, so installing does not need Rust.

## Development

```
cargo test
```

Both executables accept a hidden `--home <dir>` flag that keeps all state in a private directory with its own pipe, so a development daemon can run without installing anything:

```
target\debug\despegated.exe daemon --home .dev
target\debug\despegate.exe --home .dev status
```

Create a file named `stop` in that directory to make the daemon and its agent exit.

## Contributing

Ideas, feedback and translations are welcome through issues.

## License

[GPL-3.0](LICENSE)
