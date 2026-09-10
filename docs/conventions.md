# Conventions

## Coordinate System

Like the most of the bevy ecosystem, this project uses a right-handed Y-up coordinate
system for the game world.

- +X points right
- +Y points up
- -Z points forward, +Z points rearward
- camera: +Z points towards you, out of the screen.

## Assets

Content assets should have an alias assigned to them in a corresponding .alias
file or in _alias_rules.toml.

eg: 
- assets/myasset.txt
- assets/myasset.meta
- assets/myasset.alias

when moving an asset, their corresponding .meta and .alias should be moved with it.