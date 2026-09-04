# inr

A secure, searchable command-line vault. `inr` stores the shell commands you
run every day — with parameterized placeholders for hosts, tokens, and other
values you fill in each time — plus encrypted secrets and plain env-style
variables, all indexed with SQLite FTS5 for instant fuzzy search.

Type a few letters, arrow down to the command you want, hit enter — it runs
right there in your terminal.

## Contents

- [Features](#features)
- [Install](#install)
- [Quick start](#quick-start)
- [Command reference](#command-reference)
- [Variables and placeholders](#variables-and-placeholders)
- [The `@` lookup](#the--lookup)
- [Sharing between machines](#sharing-between-machines)
- [Security model](#security-model)
- [Data storage](#data-storage)
- [Development](#development)
- [License](#license)

## Features

- **Instant fuzzy search.** `inr s` searches your saved commands' descriptions
  and text as you type, backed by SQLite FTS5 — no scrolling through shell
  history.
- **Parameterized commands.** Save a template once (`ssh %s:host -p %n:port`)
  and get prompted for each value on every run, instead of copy-pasting
  variations of the same command.
- **Encrypted secrets.** Secret values — both saved env variables and secret
  command parameters — are encrypted at rest with AES-256-GCM, keyed by a
  master password run through Argon2id. Everything else (command text,
  descriptions, plain env values) stays in an ordinary, fully searchable
  SQLite file.
- **Secrets never touch argv or history.** A secret is injected into the
  child process's environment, not spliced into the command line — it never
  appears in `ps`/Task Manager, your shell history, or `inr`'s own execution
  log.
- **Reusable env variables.** Save a value once (text, secret, float, or
  integer) and pull it into any command parameter later by typing `@` and
  searching, instead of retyping it.
- **Runs in your real terminal.** Commands execute as an inherited child
  process through your shell (`$SHELL -c` on Unix, PowerShell on Windows) —
  full interactivity, live output, Ctrl+C all work exactly like running the
  command directly.

## Install

Requires a recent Rust toolchain (edition 2024, rustc 1.85+).

```sh
git clone <this-repo>
cd inr
cargo install --path .
```

This builds `inr` with a bundled SQLite (via `rusqlite`'s `bundled` feature),
so there's no system SQLite dependency to worry about.

## Quick start

```sh
# One-time setup: choose a master password for secrets
inr i

# Save a command with a secret and a text parameter
inr a "curl -H 'Authorization: %s:token' https://api.example.com/%t:endpoint"

# Save a reusable secret you can pull into that command later
inr env a s:apitoken

# Search and run
inr s
```

## Command reference

| Command | Description |
|---|---|
| `inr i` | Initialize `inr` — sets the master password used to encrypt secrets. Run this once. |
| `inr a <command>` | Save a new command. Any `%s:`/`%t:`/`%n:` placeholders are parsed out as variables; you're then asked for a searchable description. |
| `inr s` | Search saved commands and run the selected one. Prompts for each of its variables' values, then executes it in your terminal. |
| `inr d <id>` | Delete a saved command by its id (asks for confirmation). |
| `inr h` | Show recent execution history — timestamp, command, and which values/envs were used. Secret values are never recorded, only that a variable came from a named env (`@envname`) or was entered directly (shown as hidden). |
| `inr env a <spec>` | Add an env variable. Prefix the name to set its kind: `s:` secret, `t:` text (default), `nf:` float, `ni:` integer. |
| `inr env s` | Search env variables; selecting one copies its value to the clipboard. Secret values require the master password and auto-clear from the clipboard after ~20s. |
| `inr env r <id>` | Remove an env variable by id (asks for confirmation). |
| `inr export <path>` | Export every command and env to an encrypted transfer file, for moving them to another machine. See [Sharing between machines](#sharing-between-machines). |
| `inr import <path>` | Import commands and envs from a transfer file, merging with what's already here. See [Sharing between machines](#sharing-between-machines). |

## Variables and placeholders

A command template can contain placeholders of the form `%<kind>:<name>`:

| Placeholder | Kind | Example |
|---|---|---|
| `%s:name` | Secret — masked entry, or looked up from a saved secret env | `%s:token` |
| `%t:name` | Plain text | `%t:username` |
| `%n:name` | Number (int or float) | `%n:port` |

Placeholders are just names and kinds at save time — you're prompted for the
actual value fresh, every time you run the command from `inr s`. That's the
point: one saved template, reused with different values.

## The `@` lookup

When `inr s` prompts you for a `%t:`/`%n:` variable's value, type `@` to
search your saved env variables instead of typing a literal value — matches
are filtered to the compatible kind (a `%t:` slot only ever searches `t:`
envs, `%n:` only searches `nf:`/`ni:` envs), so a secret can never end up
somewhere it would be exposed.

For a `%s:` (secret) variable, you'll instead be asked up front whether to
type the value or look it up from your saved secrets — `inquire`, the
prompt library `inr` is built on, doesn't support a field that's masked by
default but switches to a live search mid-keystroke, so this is an explicit
menu instead of a magic character.

## Sharing between machines

`inr export`/`inr import` move your whole vault — every command and every
env — to another machine.

```sh
# On the source machine
inr export my-inr-backup.inrx

# Copy my-inr-backup.inrx to the other machine however you like
# (USB drive, cloud storage, scp, ...), then on the destination:
inr import my-inr-backup.inrx
```

- **The transfer file is its own thing, separately encrypted.** `export`
  asks you to set a *transfer passphrase* — unrelated to either machine's
  master password — and encrypts the whole file with it (same AES-256-GCM +
  Argon2id scheme used for secrets). `import` asks for that same passphrase
  to unlock it. Treat the file as sensitive until it's imported: it embeds
  the plaintext value of every secret env, protected only by that
  passphrase.
- **Secrets are re-keyed, not copied.** A secret's ciphertext on disk is
  only ever meaningful under the master password that created it. `export`
  decrypts secrets with your master password before packing them into the
  file; `import` re-encrypts each one under the destination's own master
  password. This means importing secrets requires the destination to
  already be initialized (`inr i` already run) — if it isn't, `import`
  stops and tells you to run `inr i` first.
- **Duplicates are resolved interactively.** An env is a duplicate if the
  name already exists locally; a command is a duplicate if its exact
  template text already exists (descriptions may still differ). `import`
  first shows you counts — new / conflicting / unchanged — then lets you
  resolve conflicts one at a time (**Keep mine** vs **Use incoming**), or
  apply one choice to every conflict at once (with a yes/no confirmation
  first). A conflicting secret env shows that its value differs without
  displaying either one.
- Importing is all-or-nothing per run: if you cancel partway through
  resolving conflicts, nothing is written to your database.

## Security model

- **What's encrypted:** only secret values — `s:` envs and `%s:` command
  parameters — using AES-256-GCM with a key derived from your master
  password via Argon2id (OWASP's current minimum-recommended parameters).
  Command text, descriptions, and plain env values are stored as ordinary
  plaintext in SQLite, which is what makes `inr s` and `inr env s` instant
  and password-free.
- **Password verification:** a known plaintext ("canary") is encrypted under
  your master key at `inr i` time. Unlocking later re-derives the key and
  tries to decrypt the canary — AES-GCM's authentication tag makes a wrong
  password fail cleanly rather than producing garbage.
- **No password recovery.** There is no backdoor, reset flow, or recovery
  key. If you lose the master password, every secret encrypted with it is
  unrecoverable by design.
- **Secrets never hit argv.** A secret's value is set as an environment
  variable scoped to the one child process being run, and the command line
  references it by variable name (`$INR_SECRET_1` / `$env:INR_SECRET_1`) —
  never inlined as text. This keeps it out of `ps`/Task Manager output, your
  shell's history file, and `inr`'s own history log.
- **Injection safety.** Every non-secret value is shell-quoted before being
  spliced into the command line, so a value containing `;`, `&&`, quotes, or
  `$(...)` is always treated as one literal argument, never as shell syntax.
- **Threat model.** `inr` is a local, single-user tool. It protects secrets
  at rest (on disk) and in transit through the shell (argv/history/logs). It
  does not protect against another process running as the same OS user
  while a command is actually executing, and it does not implement a
  password attempt lockout — this is a local vault, not a hosted service.

## Data storage

`inr` stores its SQLite database in your OS's standard local data
directory (via the [`directories`](https://crates.io/crates/directories)
crate):

| Platform | Path |
|---|---|
| Linux | `~/.local/share/inr/inr.db` |
| macOS | `~/Library/Application Support/inr/inr.db` |
| Windows | `%APPDATA%\inr\data\inr.db` |

## Development

```sh
cargo build
cargo test
```

Tests are unit and logic-level integration tests run against a real
in-memory SQLite database — covering crypto round-trips and wrong-password
rejection, placeholder parsing, shell-quoting/injection safety, FTS5 search
correctness, env uniqueness and kind-filtering, history logging (with an
explicit test that a secret's value is never recorded), and the transfer
file format and import merge logic (encode/decode round-trips, wrong
transfer passphrase, new/identical/conflicting classification for both
commands and envs, and secret re-encryption under a different key). The
interactive terminal UI itself isn't covered by automated tests and is
verified by hand.

## License

MIT — see [LICENSE](LICENSE).
