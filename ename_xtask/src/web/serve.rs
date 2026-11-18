//! A static file server for trying the browser build locally. Not for publishing: any web server
//! that sends `.wasm` as `application/wasm` serves the same directory (`docs/web.md`).

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
};

/// Serves `root` on `127.0.0.1:port` until the process is stopped.
pub fn serve(root: &Path, port: u16) -> Result<(), String> {
    if !root.join("index.html").is_file() {
        return Err(format!(
            "no index.html in {}: run `cargo xtask ename_web_build` first",
            root.display()
        ));
    }
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|err| format!("cannot listen on port {port}: {err}"))?;
    println!("serving {} at http://127.0.0.1:{port}/", root.display());
    for stream in listener.incoming().flatten() {
        let root = root.to_path_buf();
        std::thread::spawn(move || {
            if let Err(err) = respond(&root, stream) {
                eprintln!("ename_web_serve: {err}");
            }
        });
    }
    Ok(())
}

fn respond(root: &Path, mut stream: TcpStream) -> std::io::Result<()> {
    let mut request_line = String::new();
    BufReader::new(&stream).read_line(&mut request_line)?;
    let mut words = request_line.split_whitespace();
    let (Some(method), Some(target)) = (words.next(), words.next()) else {
        return Ok(());
    };
    if method != "GET" && method != "HEAD" {
        return reply(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"",
            true,
        );
    }
    let Some(path) = file_for(root, target) else {
        return reply(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found",
            true,
        );
    };
    match std::fs::read(&path) {
        Ok(body) => reply(
            &mut stream,
            "200 OK",
            content_type(&path),
            &body,
            method == "GET",
        ),
        Err(_) => reply(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found",
            true,
        ),
    }
}

/// The file a request names under `root`, or `None` for anything that would leave it.
fn file_for(root: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next().unwrap_or_default();
    let path = percent_decode(path)?;
    let mut file = root.to_path_buf();
    for component in Path::new(path.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => file.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if file.is_dir() {
        file.push("index.html");
    }
    file.is_file().then_some(file)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        // `WebAssembly.instantiateStreaming` refuses any other type.
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("ktx2") => "image/ktx2",
        Some("glb") => "model/gltf-binary",
        Some("gltf") => "model/gltf+json",
        _ => "application/octet-stream",
    }
}

fn reply(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    send_body: bool,
) -> std::io::Result<()> {
    // `no-cache` so a rebuild is picked up on reload.
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if send_body {
        stream.write_all(body)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_cannot_leave_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "").unwrap();
        std::fs::create_dir(root.path().join("assets")).unwrap();
        std::fs::write(root.path().join("assets/a b.png"), "").unwrap();
        assert_eq!(
            file_for(root.path(), "/"),
            Some(root.path().join("index.html"))
        );
        assert_eq!(
            file_for(root.path(), "/assets/a%20b.png?x=1"),
            Some(root.path().join("assets/a b.png"))
        );
        assert_eq!(file_for(root.path(), "/../secret"), None);
        assert_eq!(file_for(root.path(), "/assets/%2e%2e/%2e%2e/x"), None);
    }
}
