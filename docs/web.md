# The browser build

The game also runs in a browser as `wasm32-unknown-unknown`, rendered with WebGPU. Desktop stays
the main target: the web build compiles out what a browser cannot do, and nothing in the desktop
build changes for it.

## Building and trying it

```sh
cargo xtask ename_web_build            # debug build into target/web/dist
cargo xtask ename_web_serve            # http://127.0.0.1:8080/
cargo xtask ename_web_build --release  # what you publish
```

- `-p <package>` builds another binary. The default is `[workspace.metadata.ename.web] package` in
  the workspace `Cargo.toml`.
- `--webgl2` renders through WebGL2 instead. More browsers, but no compute shaders or storage
  buffers, so only a game that uses neither works.
- `--out <dir>` writes somewhere other than `target/web/dist`.
- Without `--release` the build uses the `web-dev` profile: dev without debug info. With debug
  info the module is about 800 MB, and generating its glue needs over 10 GB of memory.
- `--release` uses the `web-release` profile: release, `opt-level = "s"`, thin LTO. The player
  downloads the module, so size matters more than speed here.

The build uses its own target dir, `target/web`, so it never waits on the desktop build's lock.
`cargo web-check` and `cargo web-clippy` check the default binary for wasm the same way.

A game workspace needs both profiles in its own `Cargo.toml`, since cargo reads profiles from the
workspace root. The binary also needs a `webgpu` feature that turns on `bevy/webgpu`, and its
default features must not include `dev`: `bevy/dev` turns on the file watcher, which does not
compile for wasm. The build passes `--no-default-features`.

## What the output is

`target/web/dist` is a static site:

- `index.html`, which says so if the browser has no WebGPU.
- `game.js` and `game_bg.wasm`, the module and the glue `wasm-bindgen` generates for it.
- `assets/`, the asset tree as loose files (hard links where possible), plus
  `content-discovery.ron.gz` and `asset-metas.txt`.

Bevy fetches each asset over HTTP relative to the page. HTTP cannot list a directory, so the game
cannot walk the tree for packages the way the desktop build does. `ename_web_build` runs the walk
at build time and bakes the result to `content-discovery.ron.gz`. The game reads that file at startup
and resolves load order from it as usual (`ename_asset_package::resolve_discovery`).

Archives are not supported in the browser. The bake fails if a package lives in a `.zip` or
`.tar`; unpack it first.

## Publishing

Copy the output directory to any static web server. Two requirements:

- `.wasm` must be served as `application/wasm`, or the browser refuses to stream-compile it.
- Serve it over HTTPS (or from `localhost`): browsers only expose WebGPU in a secure context.

Precompressing `game_bg.wasm` with Brotli or gzip, where the server supports that, cuts the
download a lot.

Bevy asks for a `.meta` beside every asset it loads, and over HTTP each missing one would be a
404 and a round trip. The build lists the `.meta` files that exist in `assets/asset-metas.txt`,
and the game asks only for those.

## What the browser build leaves out

Compiled out with `cfg(not(target_arch = "wasm32"))`:

- Video recording (it runs ffmpeg) and screenshots to disk.
- Archive mounts.
- The process id in `RunId`.

These still compile but fail if used, because the browser has no filesystem:

- Saving a stage and the viewpoints file.
- The user's load order file and other persistent data: `ename_application::Paths` resolves to
  nothing, so the game runs with the default load order.

Perf sampling works, except for CPU load, which `sysinfo` cannot read in a browser. GPU pass
timings turn themselves off when the browser has no timestamp queries.

## Browser support

WebGPU ships in Chrome and Edge, and in recent Firefox and Safari. On Linux, Chrome may need
`chrome://flags/#enable-unsafe-webgpu`.
