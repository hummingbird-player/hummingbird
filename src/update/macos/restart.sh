#!/bin/sh
set -eu

app_pid="$1"
source_app="$2"
target_app="$3"
temp_root="$4"

install_dir="${target_app%/*}" # gives the parent directory, i.e. /Applications
app_name="${target_app##*/}" # gives the basename, i.e. Hummingbird.app
staged_app="$install_dir/.${app_name}.update"
backup_app="$install_dir/.${app_name}.backup"

# wait until hummingbird exits
while kill -0 "$app_pid" 2>/dev/null; do
    sleep 1
done

# copy the downloaded app into a staged location
rm -rf "$staged_app" "$backup_app"
/usr/bin/ditto "$source_app" "$staged_app"

# move the current app out of the way, then swap the staged app into place
if [ -e "$target_app" ]; then
    mv "$target_app" "$backup_app"
fi

# restore the backup if the swap fails, or delete it if it's successful
if mv "$staged_app" "$target_app"; then
    rm -rf "$backup_app"
elif [ -e "$backup_app" ]; then
    mv "$backup_app" "$target_app"
    exit 1
else
    exit 1
fi

# remove the temporary files and relaunch
rm -rf "$temp_root"
/usr/bin/open -n "$target_app"
