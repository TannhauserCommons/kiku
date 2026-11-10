# Kiku's custom folder

Voice macros: a phrase you dictate runs a list of steps in the app in front. Kiku reloads these files
within a second of saving. Kiku writes this file once and never overwrites it.

| File | Holds |
| --- | --- |
| `macros.toml` | The macros. Settings > Macros edits it too (comments and order are kept). |
| `variables.toml` | Your own values, `name = "text"`, used as `{name}`. Private: never logged or shown. |
| `snippets\` | Text files for `paste_file` and `attach`. Paths are relative to this folder, or absolute. |

`words.txt` (one folder up) stays where it is: its `>` lines are one-step macros in the same index. A `>` paste
is literal (its text is pasted exactly, `#` included) and may be said in the middle of a sentence.

## A macro

```toml
[[macro]]
name = "standup"                          # required, unique
say = ["standup", "daily update"]         # the words that run it: one phrase or a list (aliases)
hotkey = "Ctrl+Alt+S"                     # optional: a shortcut that runs it (see Triggers)
listen = false                            # optional: run when you say a phrase without the dictation key
apps = ["Discord"]                        # optional: only with these apps in front (dictating, by shortcut or listening)
modes = ["coding"]                        # optional: only in these modes (see Modes; default: every mode but off)
anywhere = false                          # optional: may be said mid-sentence (paste steps only, no slots)
enabled = true                            # optional: false lists it but never runs it
steps = [
  { paste = "Standup {date}: " },
  { press = "ctrl+k, ctrl+c" },
]
```

A macro needs `say`, `hotkey` or both. It runs when the whole dictated line is a phrase (case and punctuation aside). Slots in `say`:
`{words}` takes the rest of the line, `{n:number}` a number (number words zero to twenty or digits);
use them in steps as `{words}` and `{n}`. `{heard}` is the whole line said.

```toml
say = "go to line {n:number}"
say = "search for {words}"
```

## Triggers

Three ways to run a macro; a macro may have any of them.

- `say`: hold the dictation key and say it, as above.
- `hotkey = "Ctrl+Alt+M"`: a shortcut, written as Settings > Shortcuts writes keys (`Ctrl`, `Alt`, `Shift`, `Win`
  in any order, then one key). It needs Ctrl, Alt or Win (or is F13 to F24), never Win+L or Ctrl+Alt+Delete,
  never the captions key (Ctrl+Win by default) or it with a command key (Ctrl+Win+V), and no two macros share
  one (the later is skipped). Kiku catches it everywhere, runs the macro alone and gives the key to no app, so
  `{heard}` is empty and a macro that uses a slot can't have a shortcut. `apps` and `modes` still decide where it runs.
- `listen = true`: runs when your mic's finished line, with no key held, is one of its phrases as a whole line
  (slots work). Only your mic counts, never an app's audio. Needs `say`, and doesn't run in dictation only
  mode. The wake word, `macros.wake` in settings (Macros tab; empty by default), must start the line when
  set: "Computer, next slide."

```toml
[[macro]]
name = "next slide"
say = ["next slide", "next"]
listen = true
steps = [ { press = "right" } ]

[[macro]]
name = "sign off"
hotkey = "Ctrl+Alt+S"
steps = [ { paste = "Thanks,\n{name}" } ]
```

## Steps

Each step has exactly one of these keys. Steps run in order, 150 ms apart.

```toml
{ paste = "Hi {name},\nthanks!" }                # text, pasted (Shift+Insert), never typed key by key
{ paste_file = "snippets/reply.txt" }            # a text file, variables filled
{ paste_file = "snippets/code.txt", raw = true } # the same, braces left alone
{ attach = ["snippets/logo.png", "C:/notes.pdf"] } # files put on the clipboard and pasted as files
{ press = "ctrl+shift+t" }                       # a chord; Win+L is refused
{ press = "ctrl+k, ctrl+c" }                     # a sequence, comma between chords
{ press = "down ×3" }                            # repeat 1 to 100 times
{ press = "down ×{n}" }                          # repeat by a slot
{ copy = true }                                  # copy the selection to the clipboard
{ copy = "text with {date}" }                    # put this text on the clipboard
{ type = "text" }                                # typed key by key, only for the rare app that refuses a paste
{ wait = 500 }                                   # milliseconds
{ notify = "Done" }                              # a message in the panel (plain text, no variables)
{ mode = "commands" }                            # switch mode (see Modes)
{ open = "https://duckduckgo.com/?q={words|url}" } # open a link, file, folder or app (see Open and switch)
{ switch = "Discord" }                           # bring that app's window to the front
{ run = "search.ps1", args = ["{words}"], output = "paste" } # run your own script (see Run)
{ ask_claude = "Summarise:\n{transcript:Discord:10m|names}" } # hand this text to Claude
```

In TOML text `\n` is a line break; use `'''...'''` for several lines.

## Variables

`{name}` is a value from `variables.toml`, a slot, or a built-in below. `{name:argument}` gives one an
argument; `|filter` (or `|filter:argument`) changes the result, and filters chain: `{clipboard|trim|uppercase}`.
Write `{{` and `}}` for literal braces.

| Variable | Gives |
| --- | --- |
| `{date}`, `{date:%d.%m.%Y}` | Today, `%Y-%m-%d` unless a chrono/strftime format is given |
| `{time}`, `{time:%H:%M:%S}` | Now, `%H:%M` unless a format is given |
| `{weekday}` | Monday, Tuesday, ... |
| `{clipboard}`, `{selection}` | The clipboard; the selected text |
| `{app}`, `{window}` | The app in front; its window title |
| `{file:snippets/a.txt}` | A text file's contents |
| `{random:CHOICES}` | One of the choices, see below |
| `{transcript:WHO:HOW_MUCH}` | Captions, see below (HOW_MUCH defaults to `line`) |
| `{last_line:WHO}`, `{last_minute:WHO}` | Shorthands for `line` and `1m` |

Choices are separated by a plain `|`, e.g. `{random:yes|no|maybe}`.

WHO: `you`, `others`, `everyone`, `front` (the app in front), or an app's name as Kiku shows it.
HOW_MUCH: `line` (newest finished line), `N lines`, `Ns`, `Nm` (heard that long ago or later), `paragraph`,
`since me` (everything since your last line), `session`. Only finished lines count. `paragraph` goes back
to the last pause of 3 s or more, measured between when lines were first heard.

Filters: `lowercase`, `no-trailing-punctuation`, `capitalise` (as in `words.txt`), `uppercase`, `title`,
`trim`, `rules` (the word rules of the app in front), `url` (percent-encode), `quote` (a `> ` before each
line), `oneline`, `words:N` and `chars:N` (the first N), and for transcripts `names` (`Speaker: ` before
each line) and `times` (`[HH:MM] `). Put `names` and `times` before `words`, `chars` and `oneline`.

```toml
{ paste = "{transcript:Discord:5 lines|names|times|quote}" }
{ paste = "https://example.com/?q={selection|url}" }
```

## Modes

A mode decides what dictation does. Built in: `normal` (dictation and macros), `commands` (only macros run, each
said as the whole line, so `anywhere` does nothing there; what isn't a macro is pasted nowhere) and `off` (only the mode phrases are heard). A macro's `modes` lists where
it runs (empty: every mode but `off`; `modes = ["off"]` is refused). Add your own as tables:

```toml
[[mode]]
name = "coding"          # not normal, commands or off
apps = ["Code"]          # optional: the mode while one of these is in front

[[macro]]
name = "next error"
say = "next error"
modes = ["coding", "commands"]
steps = [ { press = "f8" } ]
```

Switch with a `{ mode = "coding" }` step, by saying a built-in phrase as the whole line (each can be turned off
in Settings > Macros): "command mode", "normal mode", "dictation off", "dictation on" (back to normal) and
"what can I say" (opens the palette). An app's own mode applies while the mode chosen is `normal`; a mode
chosen by voice or step wins. The panel's title bar shows the mode unless it is `normal`. Built-in phrases are
heard when dictating, not by a listening mic.

## Open and switch

`open` takes a link (`http`, `https` or `mailto` only), a file or folder that exists (a full path on a drive), or an
app's name. `{words|url}` encodes a slot for a link. Refused, with a notice: other kinds of link (`file:`,
`javascript:`, ...), programs, scripts and shortcuts (`.exe`, `.bat`, `.ps1`, `.py`, `.lnk` and the like: that
is what `run` is for), network paths (`\\server\share`) and anything that reads as a command line. An app's name
is looked up only among the Start menu's shortcuts and App Paths, never handed to a shell; those lists are in
your own profile, so anything that can write there can choose what a name opens (as a click on that shortcut
would). `switch = "Discord"` brings the newest top-level window of that app (named as Kiku shows apps) to the
front, or says there is none.

## Run

`run` starts one of your own scripts. It is off until Settings > Macros > "Let macros run programs" is on, and
then only for programs in `custom\scripts\` unless "from anywhere" is on too. Each exact command (program, each
argument as written, output setting) must be approved in the Macros tab, together with the script's contents:
change any of them and it needs approval again. Only you can approve, in the app; an assistant editing these
files can't. Nothing runs from captions.

```toml
[[macro]]
name  = "search my notes"
say   = "search my notes for {words}"
steps = [ { run = "search.ps1", args = ["{words}", "--quiet"], output = "paste", timeout_ms = 10000 } ]
```

- Programs: `.exe`, `.bat`, `.cmd`, `.ps1` (PowerShell, no profile) and `.py` (through `py`). A relative path
  is inside `custom\scripts\` and can't leave it. The window is hidden.
- `args`: each is one argument, filled from variables and slots, passed as it is and never through a shell, so a
  spoken `a & b; del x` stays one argument.
- `output`: `none` (default), `paste`, `notify`, or `variable:name`, which later steps use as `{name}`. At most
  1 MiB is kept, one trailing line break is dropped, and it is never logged.
- `timeout_ms`: 10000 by default, at most 60000. The script is ended at the timeout. Kiku's panel waits
  for it meanwhile, so keep scripts quick.

## Ask Claude and the palette

`ask_claude` hands its filled text to Claude as the sparkles button does (Claude Desktop, else the clipboard,
marked private). The palette (Ctrl+Win+M, or say "what can I say") lists the macros that would run with the app
in front in the mode now, each with its first phrase or shortcut; click one to run it. Settings > Macros has a
sparkles button that opens Claude with this file's instructions to make a macro with you.

Assistants connected to Kiku's MCP server have three read-only tools: `list_macros` (names, phrases, apps, modes
and step kinds, never text), `check_customization` (every problem in this folder with `file:line`) and
`try_phrase` (what a phrase would do, text masked). They can't approve or run anything.

## Limits

Text files are at most 1 MiB, UTF-8 or UTF-16 with a byte order mark; a binary file is refused ("use
attach"). `attach` takes at most 20 files. A press repeats at most 100 times. A variable named like a built-in (`date`, `time`, `clipboard`, ...) is
never used: the built-in wins, so name yours differently.

## Privacy

Values (variables, clipboard, selection, files, transcripts) and steps' text never go to the log, an
error or a notice; only a macro's name, a variable's or filter's name and a file's path do. `{selection}`
and `copy = true` make the app in front copy (Ctrl+C), which Kiku can't mark, so that text may reach Windows
clipboard history and the cloud clipboard. Text Kiku pastes or copies (`copy = "..."`) is marked to stay
out of both. Don't route `variables.toml` values through `{selection}` or `copy = true`; use `{name}`.

## Errors

A macro that can't load is skipped, the rest still work. The log (`log.txt`) says where and why, for
example `macros.toml:12 skipped: no variable or slot called city`, naming the line of its `[[macro]]`.
A step that can't run (a missing file, nothing to paste) shows a notice naming the macro and the others go on.
