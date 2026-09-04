//! Server-sent events over a blocking reader: the subset the Messages API
//! streams (`event:` + one or more `data:` lines per event, blank line
//! terminates, `:` comments ignored).

use std::io::BufRead;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Event {
    pub event: String,
    pub data: String,
}

pub struct SseReader<R: BufRead> {
    inner: R,
    line: String,
}

impl<R: BufRead> SseReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            line: String::new(),
        }
    }
}

impl<R: BufRead> Iterator for SseReader<R> {
    type Item = std::io::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut ev = Event::default();
        let mut seen = false;
        loop {
            self.line.clear();
            match self.inner.read_line(&mut self.line) {
                Ok(0) => return seen.then(|| Ok(ev)),
                Ok(_) => {}
                Err(e) => return Some(Err(e)),
            }
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if seen {
                    return Some(Ok(ev));
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "event" => ev.event = value.to_owned(),
                "data" => {
                    if !ev.data.is_empty() {
                        ev.data.push('\n');
                    }
                    ev.data.push_str(value);
                }
                _ => continue, // id, retry: unused
            }
            seen = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_and_joins_multiline_data() {
        let raw = "event: message_start\r\ndata: {\"a\":1}\r\n\r\n: keepalive\n\nevent: ping\ndata: {}\n\ndata: first\ndata: second\n\nevent: tail\ndata: x";
        let events: Vec<Event> = SseReader::new(raw.as_bytes()).map(|e| e.unwrap()).collect();
        assert_eq!(events.len(), 4);
        assert_eq!(events[0].event, "message_start");
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].event, "ping");
        assert_eq!(events[2].event, "");
        assert_eq!(events[2].data, "first\nsecond");
        assert_eq!(events[3].data, "x");
    }
}
