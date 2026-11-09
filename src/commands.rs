//! Slash commands typed in the message box: suggestions while typing, and
//! turning `/name [group] [subcommand] option:value …` into interaction options.
//!
//! Text before the first `option:` goes to the first string option, so
//! `/say hello there` works for a command with one text option.

use serde_json::{json, Value};

use crate::discord::model::{Command, CommandOption};

const SUBCOMMAND: u8 = 1;
const GROUP: u8 = 2;

/// The command name being typed: `/na` → `na`, until a space follows it.
pub fn typed_name(composer: &str) -> Option<&str> {
    let rest = composer.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// Commands matching the name being typed: prefix matches first.
pub fn suggestions<'a>(composer: &str, commands: &'a [Command], limit: usize) -> Vec<&'a Command> {
    let Some(typed) = typed_name(composer) else { return Vec::new() };
    let typed = typed.to_lowercase();
    let mut found: Vec<(bool, &Command)> = commands
        .iter()
        .filter_map(|c| {
            let name = c.name.to_lowercase();
            if name.starts_with(&typed) {
                Some((false, c))
            } else if name.contains(&typed) {
                Some((true, c))
            } else {
                None
            }
        })
        .collect();
    found.sort_by_key(|(contains, c)| (*contains, c.name.clone()));
    found.into_iter().take(limit).map(|(_, c)| c).collect()
}

/// After `/name ` (and any subcommands typed): the command and the options
/// that can follow, for a hint above the box.
pub fn current<'a>(composer: &str, commands: &'a [Command]) -> Option<(&'a Command, &'a [CommandOption])> {
    let rest = composer.strip_prefix('/')?;
    let (name, mut args) = split_word(rest);
    if args.is_empty() && !rest.ends_with(char::is_whitespace) {
        return None; // still typing the name
    }
    let command = commands.iter().find(|c| c.name == name)?;
    let mut options = command.options.as_slice();
    loop {
        let (word, after) = split_word(args);
        match options.iter().find(|o| is_sub(o) && o.name == word) {
            Some(sub) => {
                options = &sub.options;
                args = after;
            }
            None => return Some((command, options)),
        }
    }
}

/// `None` when the text isn't one of `commands` (send it as a message).
/// Otherwise the command and its `options` payload, or why it can't be sent.
pub fn invocation<'a>(text: &str, commands: &'a [Command]) -> Option<Result<(&'a Command, Value), String>> {
    let rest = text.strip_prefix('/')?;
    let (name, mut args) = split_word(rest);
    let command = commands.iter().find(|c| c.name == name)?;
    Some(build(command, &mut args).map(|options| (command, options)))
}

fn build(command: &Command, args: &mut &str) -> Result<Value, String> {
    let mut path: Vec<&CommandOption> = Vec::new();
    let mut options = command.options.as_slice();
    while options.iter().any(is_sub) {
        let (word, after) = split_word(args);
        let Some(sub) = options.iter().find(|o| is_sub(o) && o.name == word) else {
            let names: Vec<&str> = options.iter().filter(|o| is_sub(o)).map(|o| o.name.as_str()).collect();
            return Err(format!("/{} needs one of: {}", command.name, names.join(", ")));
        };
        path.push(sub);
        options = &sub.options;
        *args = after;
    }
    let mut values = Vec::new();
    for (option, raw) in assign(args, options)? {
        values.push(json!({ "type": option.kind, "name": option.name, "value": convert(option, &raw)? }));
    }
    for option in options.iter().filter(|o| o.required) {
        if !values.iter().any(|v| v["name"] == option.name.as_str()) {
            return Err(format!("/{} needs {}: ({})", command.name, option.name, option.description));
        }
    }
    let mut nested = Value::Array(values);
    for sub in path.into_iter().rev() {
        nested = json!([{ "type": sub.kind, "name": sub.name, "options": nested }]);
    }
    Ok(nested)
}

fn is_sub(option: &CommandOption) -> bool {
    matches!(option.kind, SUBCOMMAND | GROUP)
}

fn split_word(text: &str) -> (&str, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(end) => (&text[..end], text[end..].trim_start()),
        None => (text, ""),
    }
}

/// Splits `args` at `name:` markers for known options. Leading text without a
/// marker fills the first string option.
fn assign<'a>(args: &str, options: &'a [CommandOption]) -> Result<Vec<(&'a CommandOption, String)>, String> {
    let mut markers: Vec<(usize, usize, &CommandOption)> = Vec::new(); // (start, value start, option)
    for option in options {
        let marker = format!("{}:", option.name);
        let mut from = 0;
        while let Some(found) = args[from..].find(&marker) {
            let start = from + found;
            let at_word = start == 0 || args[..start].ends_with(char::is_whitespace);
            if at_word {
                markers.push((start, start + marker.len(), option));
                break;
            }
            from = start + marker.len();
        }
    }
    markers.sort_by_key(|m| m.0);
    let mut assigned = Vec::new();
    let leading = args[..markers.first().map_or(args.len(), |m| m.0)].trim();
    if !leading.is_empty() {
        let free = options
            .iter()
            .find(|o| o.kind == 3 && !markers.iter().any(|m| m.2.name == o.name))
            .or_else(|| options.iter().find(|o| !markers.iter().any(|m| m.2.name == o.name)));
        match free {
            Some(option) => assigned.push((option, leading.to_owned())),
            None if options.is_empty() => return Err("this command takes no options".to_owned()),
            None => return Err(format!("unexpected text: {leading}")),
        }
    }
    for (index, &(_, value_start, option)) in markers.iter().enumerate() {
        let end = markers.get(index + 1).map_or(args.len(), |next| next.0);
        let value = args[value_start..end].trim();
        let value = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(value);
        assigned.push((option, value.to_owned()));
    }
    Ok(assigned)
}

fn convert(option: &CommandOption, raw: &str) -> Result<Value, String> {
    let bad = || format!("{} doesn't look right: {raw}", option.name);
    Ok(match option.kind {
        4 => json!(raw.parse::<i64>().map_err(|_| bad())?),
        10 => json!(raw.parse::<f64>().map_err(|_| bad())?),
        5 => match raw.to_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => json!(true),
            "false" | "no" | "off" | "0" => json!(false),
            _ => return Err(bad()),
        },
        // Users, channels, roles: a mention or a bare id.
        6..=9 => {
            let id: String = raw.chars().filter(char::is_ascii_digit).collect();
            if id.is_empty() {
                return Err(bad());
            }
            json!(id)
        }
        11 => return Err("attachment options aren't supported yet".to_owned()),
        _ => json!(raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(kind: u8, name: &str, required: bool, options: Vec<CommandOption>) -> CommandOption {
        CommandOption { kind, name: name.to_owned(), description: String::new(), required, options }
    }

    fn command(name: &str, options: Vec<CommandOption>) -> Command {
        Command {
            id: "1".into(),
            application_id: "2".into(),
            version: "3".into(),
            name: name.into(),
            description: String::new(),
            options,
            app_name: "Bot".into(),
            raw: Value::Null,
        }
    }

    fn commands() -> Vec<Command> {
        vec![
            command("say", vec![option(3, "text", true, vec![])]),
            command(
                "remind",
                vec![option(3, "what", true, vec![]), option(4, "minutes", false, vec![]), option(6, "who", false, vec![])],
            ),
            command(
                "role",
                vec![option(1, "add", false, vec![option(8, "role", true, vec![]), option(5, "silent", false, vec![])])],
            ),
            command("ping", vec![]),
        ]
    }

    #[test]
    fn free_text_fills_the_first_string_option() {
        let commands = commands();
        let (command, options) = invocation("/say hello there", &commands).unwrap().unwrap();
        assert_eq!(command.name, "say");
        assert_eq!(options, json!([{ "type": 3, "name": "text", "value": "hello there" }]));
    }

    #[test]
    fn named_options_are_converted() {
        let commands = commands();
        let (_, options) = invocation("/remind what:\"feed the cat\" minutes:15 who:<@42>", &commands).unwrap().unwrap();
        assert_eq!(
            options,
            json!([
                { "type": 3, "name": "what", "value": "feed the cat" },
                { "type": 4, "name": "minutes", "value": 15 },
                { "type": 6, "name": "who", "value": "42" },
            ])
        );
    }

    #[test]
    fn subcommands_nest_their_options() {
        let commands = commands();
        let (_, options) = invocation("/role add role:<@&7> silent:yes", &commands).unwrap().unwrap();
        assert_eq!(
            options,
            json!([{ "type": 1, "name": "add", "options": [
                { "type": 8, "name": "role", "value": "7" },
                { "type": 5, "name": "silent", "value": true },
            ] }])
        );
        assert!(invocation("/role", &commands).unwrap().unwrap_err().contains("add"));
    }

    #[test]
    fn problems_are_reported_and_unknown_commands_are_text() {
        let commands = commands();
        assert!(invocation("/remind minutes:soon what:x", &commands).unwrap().is_err());
        assert!(invocation("/remind minutes:5", &commands).unwrap().unwrap_err().contains("what"));
        assert!(invocation("/ping extra", &commands).unwrap().is_err());
        assert_eq!(invocation("/ping", &commands).unwrap().unwrap().1, json!([]));
        assert!(invocation("/shrug", &commands).is_none());
        assert!(invocation("not a command", &commands).is_none());
    }

    #[test]
    fn suggests_while_typing_and_hints_after() {
        let commands = commands();
        let names: Vec<&str> = suggestions("/r", &commands, 8).iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["remind", "role"]);
        assert!(suggestions("/remind ", &commands, 8).is_empty());
        let (command, options) = current("/role add ", &commands).unwrap();
        assert_eq!((command.name.as_str(), options.len()), ("role", 2));
        assert!(current("/rol", &commands).is_none());
    }
}
