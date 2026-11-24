#!/usr/bin/env python3
"""Regenerate the Finder layout template. Requires ds-store==1.3.1.

This is an asset authoring tool. Normal packaging only copies the template.
"""
from pathlib import Path

from ds_store import DSStore


def main():
    target = Path(__file__).resolve().parent / 'assets' / 'dmg-layout.dsstore'
    target.parent.mkdir(parents=True, exist_ok=True)
    with DSStore.open(str(target), 'w+') as store:
        store['.']['bwsp'] = {
            'ShowToolbar': False,
            'ShowStatusBar': False,
            'ShowPathbar': False,
            'ShowSidebar': False,
            'ShowTabView': False,
            'WindowBounds': '{{400, 200}, {660, 400}}',
        }
        store['.']['icvp'] = {
            'viewOptionsVersion': 1,
            'backgroundType': 1,
            'backgroundColorRed': 1.0,
            'backgroundColorGreen': 1.0,
            'backgroundColorBlue': 1.0,
            'iconSize': 100.0,
            'textSize': 12.0,
            'gridSpacing': 100.0,
            'gridOffsetX': 0.0,
            'gridOffsetY': 0.0,
            'arrangeBy': 'none',
            'labelOnBottom': True,
            'showIconPreview': True,
            'showItemInfo': False,
            'scrollPositionX': 0.0,
            'scrollPositionY': 0.0,
        }
        store['.']['vSrn'] = ('long', 1)
        store['.']['icvl'] = ('type', b'icnv')
        store['MeterNest.app']['Iloc'] = (180, 170)
        store['Applications']['Iloc'] = (480, 170)
    print(target)


if __name__ == '__main__':
    main()
