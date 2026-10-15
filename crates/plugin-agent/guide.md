# Agent guide: {name}

You're connected to one running instance of {name}, a plug-in built on Loombridge's plug-in kit, inside a host (a digital audio workstation). The person switched on this instance's agent server and gave you its token. This guide is the resource `lspk://guide`; where {name} has a guide of its own, to its modules and how to use them, it's the resource `lspk://plugin-guide`.

## Tools

| Tool | Use it to |
|---|---|
| `query` | Read the plug-in now: what it is, the host and track it's on, a summary in prose, and every parameter with its range, default and value. Start here. |
| `list_commands` | See the commands by area (`params`, `agent`, and any of the plug-in's own), or search them by words. |
| `describe_command` | Read a command's description and argument schema before running it for the first time. |
| `run` | Run one command, or several as one step. Set `dry_run` to check without changing anything. |
| `checkpoint` | Save the plug-in's state under a name, try something, and restore it if you don't like the result. |
| `preview` | Hear it: a note, a chord or a short phrase (an instrument), or a test signal (an effect), rendered offline as a WAV file with an analysis. |

## Parameters

Every parameter has an ID that never changes, a name, a kind and a range. Values are in each parameter's own units: a gain is a linear factor (1 is 0 dB, 0 is silence), a toggle is 0 or 1, a choice is its index from 0, and continuous parameters are in their unit (hertz, milliseconds, per cent). Each value also has the text the host shows, such as `-6.0 dB`.

To change parameters, run `params.set`:

```json
{ "commands": [ { "command": "params.set", "args": { "values": [
  { "param": "Gain", "text": "-6 dB" },
  { "param": 2, "value": 0.5 } ] } } ] }
```

Name a parameter by its ID, its name or its path. Give each new value as `value` (a number in its own units) or `text` (as the host would show it). Values outside the range are clamped.

## Modules and slots

A plug-in made of modules names each parameter by its module: `Osc A Level`, with the path `osc "A" / param "Level"`. Some modules are slots, such as an effects rack's: the slot's `Type` parameter chooses which module it holds, and its other parameters are that module's, with their real names and ranges. When the type changes, the slot's parameters become the new module's, at its defaults. In one `params.set`, values apply in order, so set the type first and then the new module's parameters:

```json
{ "values": [
  { "param": "fx \"1\" / param \"Type\"", "text": "Delay" },
  { "param": "fx \"1\" / param \"Time\"", "text": "250 ms" } ] }
```

## The host stays in step

Your changes reach the host exactly as if the person had moved the controls: the host records them, so its automation, its undo history and the saved project match what you hear. Everything one `run` call changes is one step. The person can undo it in the host.

If the host is playing automation on a parameter, the automation may move it back; say so to the person rather than fighting it.

## Hearing what you've done

`preview` never touches the instance the host plays: it renders a separate copy, set as this one is, offline. Give an instrument `notes` (each a `key`, where 60 is middle C and 69 is A4 at 440 Hz, a `start` and `length` in seconds, and a `velocity` from 0 to 1); give an effect an `input` (a sine, sweep, noise, impulse or silence). The analysis reports the peak and RMS level, the pitch and its nearest note, the spectrum in octave bands with its centroid, and when the sound starts, peaks and dies away.

Listen after each meaningful change, and compare against a checkpoint when you're unsure a change helped.

## Good practice

- Read before you write: `query`, then change, then `preview`.
- Make one musical change per `run`, so the person can undo it on its own.
- Save a `checkpoint` before trying something bold.
- Commands that only a person can run (the agent server's own) are refused to you; commands that ask first wait for the person's answer.
