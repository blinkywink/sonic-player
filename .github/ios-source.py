"""Writes the iPhone app source (AltStore format, also read by KravaSign, ESign,
Feather and other signers) for a release: python ios-source.py VERSION IPA OUT"""

import json
import os
import sys
from datetime import datetime, timezone

version, ipa, out = sys.argv[1:4]
repo = "https://github.com/blinkywink/sonic-player"
download = f"{repo}/releases/download/v{version}/{os.path.basename(ipa)}"
icon = "https://raw.githubusercontent.com/blinkywink/sonic-player/main/mobile/assets/AppIcon1024.png"
size = os.path.getsize(ipa)
date = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
notes = f"Sonic {version}"

source = {
    "name": "Sonic",
    "identifier": "com.sonic.player.source",
    "sourceURL": f"{repo}/releases/latest/download/source.json",
    "iconURL": icon,
    "apps": [
        {
            "name": "Sonic",
            "bundleIdentifier": "com.sonic.player.mobile",
            "developerName": "blinky",
            "subtitle": "Your Plex music, gapless.",
            "localizedDescription": "Sonic plays your Plex music library.",
            "iconURL": icon,
            "tintColor": "#6E8BFF",
            "category": "entertainment",
            "versions": [
                {
                    "version": version,
                    "date": date,
                    "localizedDescription": notes,
                    "downloadURL": download,
                    "size": size,
                    "minOSVersion": "15.0",
                }
            ],
            # Older format, for signers that read these fields directly.
            "version": version,
            "versionDate": date,
            "versionDescription": notes,
            "downloadURL": download,
            "size": size,
        }
    ],
    "news": [],
}

with open(out, "w") as file:
    json.dump(source, file, indent=2)
