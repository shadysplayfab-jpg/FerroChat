//! Login captchas. Discord sometimes asks for an hCaptcha, which only runs in
//! a browser. A second copy of the exe (`--captcha`) opens a small WebView2
//! window (Windows' Edge engine) for it, prints the answer and exits, so the
//! app itself never loads a browser engine.
//!
//! The page is a Discord address (hCaptcha only works on the site its key
//! belongs to), with its content replaced by the captcha before it shows.

/// Starts the helper; followed by the site key and the rqdata ("" for none).
pub const ARG: &str = "--captcha";

/// Opens the helper and waits for the answer. `None`: the window was closed.
pub async fn solve(sitekey: String, rqdata: Option<String>) -> Result<Option<String>, String> {
    let run = move || -> Result<Option<String>, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let output = std::process::Command::new(exe)
            .args([ARG, &sitekey, rqdata.as_deref().unwrap_or_default()])
            .output()
            .map_err(|e| format!("couldn't open the captcha window: {e}"))?;
        let answer = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        match output.status.code() {
            Some(0) if !answer.is_empty() => Ok(Some(answer)),
            Some(1) => Ok(None),
            _ => {
                let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                Err(if error.is_empty() { "the captcha window failed".to_owned() } else { error })
            }
        }
    };
    tokio::task::spawn_blocking(run).await.map_err(|e| e.to_string())?
}

/// The helper process: shows the captcha, prints its answer, exits 0 (answer),
/// 1 (closed) or 2 (couldn't open).
#[cfg(windows)]
pub fn run_helper(sitekey: &str, rqdata: &str) -> ! {
    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoop};
    use tao::window::WindowBuilder;
    use wry::{WebContext, WebViewBuilder};

    let fail = |error: String| -> ! {
        eprintln!("{error}");
        std::process::exit(2)
    };
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Ferrochat: prove you're human")
        .with_inner_size(LogicalSize::new(440.0, 640.0))
        .build(&event_loop)
        .unwrap_or_else(|e| fail(e.to_string()));
    // WebView2 keeps its files here, not next to the exe.
    let data = dirs::data_local_dir().map(|dir| dir.join("ferrochat").join("captcha"));
    let mut context = WebContext::new(data);
    let _webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url("https://discord.com/robots.txt")
        .with_initialization_script(page_script(sitekey, rqdata))
        .with_ipc_handler(|request| {
            println!("{}", request.body());
            std::process::exit(0)
        })
        .build(&window)
        .unwrap_or_else(|e| fail(format!("Windows' WebView2 isn't available ({e}). Install it from microsoft.com/edge/webview2, or use a token.")));
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            std::process::exit(1);
        }
    })
}

#[cfg(not(windows))]
pub fn run_helper(_sitekey: &str, _rqdata: &str) -> ! {
    eprintln!("captchas are only supported on Windows");
    std::process::exit(2)
}

/// Replaces the page with the captcha, and sends its answer back to the helper.
fn page_script(sitekey: &str, rqdata: &str) -> String {
    let quote = |s: &str| serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned());
    format!(
        // Also runs in hCaptcha's own frames, which must be left alone.
        r#"if (window.top === window && location.hostname === 'discord.com') window.addEventListener('DOMContentLoaded', () => {{
  document.documentElement.innerHTML = '<head><meta charset="utf-8"><title>Captcha</title></head>'
    + '<body style="margin:0;height:100vh;display:flex;align-items:center;justify-content:center;background:#17141c;color:#eee8f5;font:15px Segoe UI,sans-serif">'
    + '<div style="text-align:center"><p>Discord wants to check you\'re human<br>before logging in to Ferrochat.</p><div id="captcha"></div></div></body>';
  window.ferrochatLoaded = () => {{
    const id = hcaptcha.render('captcha', {{ sitekey: {sitekey}, theme: 'dark', callback: answer => window.ipc.postMessage(answer) }});
    if ({rqdata}) hcaptcha.setData(id, {{ rqdata: {rqdata} }});
  }};
  const script = document.createElement('script');
  script.src = 'https://js.hcaptcha.com/1/api.js?render=explicit&onload=ferrochatLoaded';
  document.head.appendChild(script);
}});"#,
        sitekey = quote(sitekey),
        rqdata = quote(rqdata),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn page_script_quotes_its_values() {
        let script = super::page_script("key\"');alert(1)//", "");
        assert!(script.contains(r#"sitekey: "key\"');alert(1)//""#), "the site key stays one string");
        assert!(script.contains(r#"if ("")"#), "no rqdata, so none is set");
    }
}
