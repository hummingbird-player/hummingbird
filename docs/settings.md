# Settings
Hummingbird can be configured with a `settings.json` file located in the following places:

| Platform | Location                                                              |
|----------|-----------------------------------------------------------------------|
| Linux    | `~/.local/share/hummingbird/settings.json`                            |
| macOS    | `~/Library/Application Support/org.mailliw.hummingbird/settings.json` |
| Windows  | `%appdata%\mailliw\hummingbird\data\settings.json`                    |

> [!NOTE]
> The default data directory was chanaged when Muzak was renamed to Hummingbird.
>
> If you first opened the application before the name change, your configuration files may
> be in the previous location.
>
> <details>
> <summary>Legacy (pre-Hummingbird) folder location</summary>
> <br>
>
> | Platform | Location                                                          |
> |----------|-------------------------------------------------------------------|
> | Linux    | `~/.local/share/muzak/settings.json`                              |
> | macOS    | `~/Library/Application Support/me.william341.muzak/settings.json` |
> | Windows  | `%appdata%\william341\muzak\data\settings.json`                   |
>
> This can be applied to all paths - they have all been changed in the same manner.
> </details>

## Example

```json
{
  "interface": {
    "theme": "themes/mytheme.json"
  },
  "scanning": {
    "paths": ["/home/me/Music", "/home/me/other"]
  },
  "playback": {
    "always_repeat": true,
    "prev_track_jump_first": true
  }
}
```

## Interface settings

### `interface.theme`

Controls the selected theme.

- `null` or omitted: use the built-in default theme
- `"themes/<name>.json"`: use a custom theme file from the `themes/` directory

You can change this from **Settings > Interface > Theme**. Theme changes apply
immediately.

## Logs
Hummingbird writes logs to stderr and also saves them to `hummingbird.log`.
You can open the current log from the command palette with the **Hummingbird: Open Log** action.

By default, logs are stored here:

| Platform | File path                                                               |
|----------|-------------------------------------------------------------------------|
| Linux    | `~/.local/state/hummingbird/hummingbird.log`                            |
| macOS    | `~/Library/Application Support/org.mailliw.hummingbird/hummingbird.log` |
| Windows  | `%localappdata%\mailliw\hummingbird\data\hummingbird.log`               |

You can choose a different folder by setting `HUMMINGBIRD_LOG_DIR`.

## Services
Service integrations are managed from **Settings > Services**.

### Discord Rich Presence
Use **Settings > Services > Discord Rich Presence** to control whether Hummingbird
shows the currently playing track in your Discord status.

### Last.FM
Connect your Last.FM account from **Settings > Services > Last.FM**.

The current Last.FM session is stored in the following places:

| Platform | Location                                                            |
|----------|---------------------------------------------------------------------|
| Linux    | `~/.local/share/hummingbird/lastfm.json`                            |
| macOS    | `~/Library/Application Support/org.mailliw.hummingbird/lastfm.json` |
| Windows  | `%appdata%\mailliw\hummingbird\data\lastfm.json`                    |

Deleting this file will disconnect your Last.FM account. This file should not
be modified manually - it will be generated when you connect your Last.FM
account.

### ListenBrainz
Connect your ListenBrainz account from **Settings > Services > ListenBrainz**.
You can find your user token at <https://listenbrainz.org/settings/>.

The current ListenBrainz session is stored in the following places:

| Platform | Location                                                                  |
|----------|---------------------------------------------------------------------------|
| Linux    | `~/.local/share/hummingbird/listenbrainz.json`                            |
| macOS    | `~/Library/Application Support/org.mailliw.hummingbird/listenbrainz.json` |
| Windows  | `%appdata%\mailliw\hummingbird\data\listenbrainz.json`                    |
