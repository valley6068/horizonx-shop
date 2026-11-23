# HorizonX Shop

A DonutSMP-style shop plugin for HorizonX servers, written in Rust. No Java.

`/shop` opens the main menu. Each category opens a shop page. Left-click an item to buy it and right-click to sell it, with -10/-1/+1/+10/set buttons. `/sell` opens a chest that pays for what you put in. `/worth` tells what the item in your hand is worth. Money comes from Essentials.

Prices move: items with `dynamic-pricing` get dearer with each one bought and cheaper with each one sold, inside a min and max. On top of that, a market moves every item's value every 20 minutes: mostly small moves, sometimes a boom or a crash, then a drift back.

## Install

Download `donutshop_rs.dll` from the latest release. With HorizonX closed, put it in the server's `plugins` folder (replace the old one), then start the server. The files it reads are in `plugins/DonutShop/`: `config.yml` (main menu, market, messages) and `shops/*.yml` (one file per shop page). Change them and run `/shop reload`.

## Build

GitHub builds it on every push (`.github/workflows/build.yml`) against the HorizonX source, runs the tests and puts the dll in a pre-release.

## Credits

The design follows Genius Shop by Andreas Jensen (MIT). See NOTICE.md.
