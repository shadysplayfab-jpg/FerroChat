//! Desktop notifications (Windows toasts).

/// Longest body shown in a toast.
const BODY_LIMIT: usize = 180;

pub async fn show(title: String, body: String) -> Result<(), String> {
    let body = truncate(&body, BODY_LIMIT);
    tokio::task::spawn_blocking(move || {
        notify_rust::Notification::new()
            .appname("Ferrochat")
            .summary(&title)
            .body(&body)
            .show()
            .map(drop)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn truncate(text: &str, limit: usize) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match single_line.char_indices().nth(limit) {
        Some((cut, _)) => format!("{}…", &single_line[..cut]),
        None => single_line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("a\n b  c", 10), "a b c");
        assert_eq!(truncate("héllo wörld", 5), "héllo…");
    }

    #[tokio::test]
    #[ignore = "shows a real desktop notification"]
    async fn shows_toast() {
        show("Ferrochat".to_owned(), "Test notification: it works!".to_owned()).await.unwrap();
    }
}
