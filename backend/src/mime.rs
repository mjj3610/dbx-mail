//! MIME 解析：把服务器上的原始邮件拆成标题、地址、正文与附件。
//!
//! 正文里的 `cid:` 内嵌图片会被替换成 data URL，这样前端在
//! `img-src data: blob:` 的沙箱里也能直接显示，不必再发一次请求。

use base64::Engine;
use mail_parser::{Address, MessageParser, MimeHeaders};
use sha2::{Digest, Sha256};

use crate::db::HeaderRow;
use crate::model::Attachment;

/// 内嵌图片总量上限（超过后不再内联，避免正文过大）
const INLINE_BUDGET: usize = 8 * 1024 * 1024;
/// 单张内嵌图片上限
const INLINE_PART_LIMIT: usize = 5 * 1024 * 1024;

/// 解析上下文：来自 IMAP 的元信息
pub struct ParseContext {
    pub account_id: i64,
    pub mailbox_id: i64,
    pub uid: Option<i64>,
    pub internal_date: i64,
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub size: i64,
}

/// 解析结果
pub struct Parsed {
    pub header: HeaderRow,
    pub text: String,
    pub html: String,
    pub calendar: String,
    pub attachments: Vec<Attachment>,
}

/// 完整解析原始邮件（含正文与附件元信息）
pub fn parse(raw: &[u8], ctx: &ParseContext) -> Option<Parsed> {
    let message = MessageParser::default().parse(raw)?;

    let subject = message.subject().unwrap_or("").trim().to_string();
    let (from_name, from_addr) = message
        .from()
        .and_then(|address| address.first())
        .map(|addr| {
            (
                addr.name().unwrap_or("").trim().to_string(),
                addr.address().unwrap_or("").trim().to_string(),
            )
        })
        .unwrap_or_default();
    let to_addrs = format_addresses(message.to());
    let cc_addrs = format_addresses(message.cc());
    let reply_to = message
        .reply_to()
        .and_then(|address| address.first())
        .and_then(|addr| addr.address())
        .unwrap_or("")
        .to_string();

    let message_id = clean_id(message.message_id().unwrap_or(""));
    let references = header_list(message.references());
    let in_reply_to = header_list(message.in_reply_to()).into_iter().next().unwrap_or_default();

    let date = message
        .date()
        .map(|value| value.to_timestamp() * 1000)
        .filter(|value| *value > 0)
        .unwrap_or(ctx.internal_date);
    let date = if date > 0 { date } else { crate::now_ms() };

    // 正文
    let raw_text = message.body_text(0).map(|value| value.into_owned()).unwrap_or_default();
    let mut html = message.body_html(0).map(|value| value.into_owned()).unwrap_or_default();
    let mut calendar = String::new();

    // 附件与内嵌图片
    let mut attachments: Vec<Attachment> = Vec::new();
    let mut cid_images: Vec<(String, String)> = Vec::new();
    let mut inline_budget = INLINE_BUDGET;

    for (index, part) in message.parts.iter().enumerate() {
        let mime = part
            .content_type()
            .map(|value| {
                format!(
                    "{}/{}",
                    value.ctype(),
                    value.subtype().unwrap_or("octet-stream")
                )
            })
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let name = part.attachment_name().unwrap_or("").trim().to_string();
        let cid = part
            .content_id()
            .unwrap_or("")
            .trim()
            .trim_matches(|c| c == '<' || c == '>')
            .to_string();
        let disposition = part.content_disposition();
        let is_attachment = disposition.map(|value| value.is_attachment()).unwrap_or(false);
        let is_inline = disposition.map(|value| value.is_inline()).unwrap_or(false);
        let contents = part.contents();

        // 纯正文部件不列为附件
        let is_body = (mime == "text/plain" || mime == "text/html")
            && name.is_empty()
            && !is_attachment
            && cid.is_empty();
        if is_body {
            continue;
        }

        // 日历邀请单独存一份，供阅读器展示会议信息
        if mime.starts_with("text/calendar") && name.is_empty() {
            calendar = String::from_utf8_lossy(contents).to_string();
            continue;
        }

        let is_image = mime.starts_with("image/");
        let keep = is_attachment || !name.is_empty() || (is_image && !cid.is_empty());
        if !keep {
            continue;
        }

        // 内嵌图片转 data URL，供 HTML 内的 cid: 引用
        if is_image && !cid.is_empty() && contents.len() <= INLINE_PART_LIMIT && inline_budget >= contents.len() {
            let encoded = base64::engine::general_purpose::STANDARD.encode(contents);
            cid_images.push((cid.clone(), format!("data:{mime};base64,{encoded}")));
            inline_budget -= contents.len();
        }

        attachments.push(Attachment {
            index: index as i64,
            filename: if name.is_empty() {
                fallback_name(&mime, index)
            } else {
                name
            },
            mime,
            size: contents.len() as i64,
            content_id: cid,
            inline: is_inline,
        });
    }

    if !html.is_empty() && !cid_images.is_empty() {
        html = inline_cid(&html, &cid_images);
    }

    let text = if raw_text.trim().is_empty() {
        html_to_text(&html)
    } else {
        raw_text
    };
    let snippet = make_snippet(&text, &html);
    let has_attachments = attachments.iter().any(|item| !item.inline);
    let thread_id = thread_key(&references, &in_reply_to, &message_id, &subject);

    Some(Parsed {
        header: HeaderRow {
            account_id: ctx.account_id,
            mailbox_id: ctx.mailbox_id,
            uid: ctx.uid,
            message_id,
            thread_id,
            subject: if subject.is_empty() { "(无主题)".into() } else { subject },
            from_name,
            from_addr,
            to_addrs,
            cc_addrs,
            reply_to,
            in_reply_to,
            references,
            date,
            internal_date: ctx.internal_date,
            size: if ctx.size > 0 { ctx.size } else { raw.len() as i64 },
            snippet,
            seen: ctx.seen,
            flagged: ctx.flagged,
            answered: ctx.answered,
            draft: ctx.draft,
            has_attachments,
        },
        text,
        html,
        calendar,
        attachments,
    })
}

/// 提取 Bcc 收件人（仅发送回退路径用：compose 写入 Bcc 头，Graph 草稿需要 bccRecipients）
pub fn parse_bcc(raw: &[u8]) -> Vec<String> {
    let Some(message) = MessageParser::default().parse(raw) else {
        return Vec::new();
    };
    format_addresses(message.bcc())
}

/// 只解析头部（同步列表用，避免下载整封邮件）
pub fn parse_header(raw: &[u8], ctx: &ParseContext) -> Option<HeaderRow> {
    let message = MessageParser::default().parse(raw)?;

    let subject = message.subject().unwrap_or("").trim().to_string();
    let (from_name, from_addr) = message
        .from()
        .and_then(|address| address.first())
        .map(|addr| {
            (
                addr.name().unwrap_or("").trim().to_string(),
                addr.address().unwrap_or("").trim().to_string(),
            )
        })
        .unwrap_or_default();
    let to_addrs = format_addresses(message.to());
    let cc_addrs = format_addresses(message.cc());
    let reply_to = message
        .reply_to()
        .and_then(|address| address.first())
        .and_then(|addr| addr.address())
        .unwrap_or("")
        .to_string();
    let message_id = clean_id(message.message_id().unwrap_or(""));
    let references = header_list(message.references());
    let in_reply_to = header_list(message.in_reply_to()).into_iter().next().unwrap_or_default();
    let date = message
        .date()
        .map(|value| value.to_timestamp() * 1000)
        .filter(|value| *value > 0)
        .unwrap_or(ctx.internal_date);
    let date = if date > 0 { date } else { crate::now_ms() };
    let has_attachments = message
        .parts
        .iter()
        .any(|part| part.content_disposition().map(|value| value.is_attachment()).unwrap_or(false));
    let snippet = String::new();
    let thread_id = thread_key(&references, &in_reply_to, &message_id, &subject);

    Some(HeaderRow {
        account_id: ctx.account_id,
        mailbox_id: ctx.mailbox_id,
        uid: ctx.uid,
        message_id,
        thread_id,
        subject: if subject.is_empty() { "(无主题)".into() } else { subject },
        from_name,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        in_reply_to,
        references,
        date,
        internal_date: ctx.internal_date,
        size: ctx.size,
        snippet,
        seen: ctx.seen,
        flagged: ctx.flagged,
        answered: ctx.answered,
        draft: ctx.draft,
        has_attachments,
    })
}

/// 从原始邮件里取出第 n 个附件（按解析顺序，与 Attachment::index 对应）
pub fn part_bytes(raw: &[u8], index: i64) -> Option<(String, Vec<u8>, String)> {
    let message = MessageParser::default().parse(raw)?;
    let part = message.parts.get(index as usize)?;
    let mime = part
        .content_type()
        .map(|value| format!("{}/{}", value.ctype(), value.subtype().unwrap_or("octet-stream")))
        .unwrap_or_else(|| "application/octet-stream".into());
    let name = part
        .attachment_name()
        .map(str::to_string)
        .unwrap_or_else(|| fallback_name(&mime, index as usize));
    Some((name, part.contents().to_vec(), mime))
}

/* ---------------- 辅助 ---------------- */

fn format_addresses(address: Option<&Address<'_>>) -> Vec<String> {
    let Some(address) = address else {
        return Vec::new();
    };
    let mut items = Vec::new();
    if let Some(list) = address.as_list() {
        for addr in list {
            let email = addr.address().unwrap_or("").trim();
            if email.is_empty() {
                continue;
            }
            let name = addr.name().unwrap_or("").trim();
            items.push(if name.is_empty() {
                email.to_string()
            } else {
                format!("{name} <{email}>")
            });
        }
    }
    items
}

/// 去掉尖括号并按空白拆成多个 message-id
fn header_list(value: &mail_parser::HeaderValue<'_>) -> Vec<String> {
    value
        .clone()
        .into_text_list()
        .unwrap_or_default()
        .iter()
        .flat_map(|item| item.split_whitespace().map(|part| clean_id(part)).collect::<Vec<_>>())
        .filter(|item| !item.is_empty())
        .collect()
}

fn clean_id(value: &str) -> String {
    value.trim().trim_matches(|c| c == '<' || c == '>').trim().to_string()
}

/// 会话键：优先 References 根节点，其次 In-Reply-To，再次 Message-ID；
/// 三者都缺失时按主题哈希兜底，避免同一话题散成一封封独立邮件。
fn thread_key(references: &[String], in_reply_to: &str, message_id: &str, subject: &str) -> String {
    if let Some(root) = references.first() {
        if !root.is_empty() {
            return root.clone();
        }
    }
    if !in_reply_to.is_empty() {
        return in_reply_to.to_string();
    }
    if !message_id.is_empty() {
        return message_id.to_string();
    }
    let mut hasher = Sha256::new();
    hasher.update(normalize_subject(subject).as_bytes());
    format!("s:{:x}", hasher.finalize())[..18].to_string()
}

fn normalize_subject(subject: &str) -> String {
    crate::db::clean_subject(subject).to_lowercase()
}

fn fallback_name(mime: &str, index: usize) -> String {
    let ext = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/calendar" => "ics",
        "message/rfc822" => "eml",
        "application/zip" => "zip",
        _ => "bin",
    };
    format!("附件-{}.{ext}", index + 1)
}

/// 把 HTML 里的 cid: 引用替换成 data URL
fn inline_cid(html: &str, images: &[(String, String)]) -> String {
    let mut output = String::with_capacity(html.len());
    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(offset) = lower[cursor..].find("cid:") else {
            output.push_str(&html[cursor..]);
            break;
        };
        let start = cursor + offset;
        output.push_str(&html[cursor..start]);
        let id_start = start + 4;
        let mut id_end = id_start;
        while id_end < bytes.len() {
            let ch = bytes[id_end] as char;
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | ')' | '>' | '<') {
                break;
            }
            id_end += 1;
        }
        let candidate = html[id_start..id_end]
            .trim()
            .trim_matches(|c| c == '<' || c == '>');
        match images.iter().find(|(cid, _)| cid.eq_ignore_ascii_case(candidate)) {
            Some((_, data_url)) => output.push_str(data_url),
            None => output.push_str(&html[start..id_end]),
        }
        cursor = id_end;
    }
    output
}

/// 粗略把 HTML 转成纯文本（用于搜索与纯文本视图）
pub fn html_to_text(html: &str) -> String {
    let mut output = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_style = false;
    let mut tag = String::new();
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let lower = tag.to_lowercase();
                if lower.starts_with("style") || lower.starts_with("script") {
                    in_style = true;
                }
                if lower.starts_with("/style") || lower.starts_with("/script") {
                    in_style = false;
                }
                if lower.starts_with("br") || lower.starts_with("/p") || lower.starts_with("/div") || lower.starts_with("/tr") {
                    output.push('\n');
                }
                tag.clear();
            }
            _ if in_tag => tag.push(ch),
            _ if !in_style => output.push(ch),
            _ => {}
        }
    }
    let decoded = output
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn make_snippet(text: &str, html: &str) -> String {
    let source = if text.trim().is_empty() { html_to_text(html) } else { text.to_string() };
    let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 180 {
        flat
    } else {
        flat.chars().take(180).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一封带 HTML、内嵌图片和 PDF 附件的邮件
    const RAW: &[u8] = b"From: Alice <alice@example.com>\r\n\
To: Bob <bob@example.com>\r\n\
Cc: carol@example.com\r\n\
Subject: =?UTF-8?B?5L2g5aW977yM5LiW55WM?=\r\n\
Date: Tue, 3 Nov 2026 10:00:00 +0800\r\n\
Message-ID: <abc@example.com>\r\n\
References: <root@example.com> <abc@example.com>\r\n\
In-Reply-To: <abc@example.com>\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: multipart/alternative; boundary=\"b2\"\r\n\
\r\n\
--b2\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
hello world\r\n\
--b2\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<html><body><p>hello <img src=\"cid:img1\"></p></body></html>\r\n\
--b2--\r\n\
--b1\r\n\
Content-Type: image/png\r\n\
Content-ID: <img1>\r\n\
Content-Disposition: inline; filename=\"pixel.png\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--b1\r\n\
Content-Type: application/pdf\r\n\
Content-Disposition: attachment; filename=\"report.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQ=\r\n\
--b1--\r\n";

    fn context() -> ParseContext {
        ParseContext {
            account_id: 1,
            mailbox_id: 2,
            uid: Some(7),
            internal_date: 1_700_000_000_000,
            seen: false,
            flagged: true,
            answered: false,
            draft: false,
            size: RAW.len() as i64,
        }
    }

    #[test]
    fn parses_headers_and_body() {
        let parsed = parse(RAW, &context()).expect("邮件应可解析");
        assert_eq!(parsed.header.subject, "你好，世界");
        assert_eq!(parsed.header.from_addr, "alice@example.com");
        assert_eq!(parsed.header.from_name, "Alice");
        assert_eq!(parsed.header.to_addrs, vec!["Bob <bob@example.com>"]);
        assert_eq!(parsed.header.thread_id, "root@example.com");
        assert!(parsed.header.flagged);
        assert!(parsed.header.has_attachments);
        assert!(parsed.text.contains("hello world"));
        assert!(parsed.html.contains("data:image/png;base64,"));
        assert!(!parsed.html.contains("cid:img1"));
        assert_eq!(parsed.attachments.len(), 2);
        assert!(parsed.attachments.iter().any(|item| item.filename == "report.pdf"));
        assert!(parsed.attachments.iter().any(|item| item.inline && item.filename == "pixel.png"));
    }

    #[test]
    fn parses_headers_only() {
        let header = b"From: Alice <alice@example.com>\r\n\
To: Bob <bob@example.com>\r\n\
Subject: Status\r\n\
Date: Tue, 3 Nov 2026 10:00:00 +0800\r\n\
Message-ID: <m1@example.com>\r\n\
\r\n";
        let row = parse_header(header, &context()).expect("头部应可解析");
        assert_eq!(row.subject, "Status");
        assert_eq!(row.thread_id, "m1@example.com");
        assert!(row.snippet.is_empty());
    }

    #[test]
    fn extracts_attachment_bytes() {
        let parsed = parse(RAW, &context()).expect("邮件应可解析");
        let target = parsed
            .attachments
            .iter()
            .find(|item| item.filename == "report.pdf")
            .expect("应找到 PDF 附件");
        let (name, bytes, mime) = part_bytes(RAW, target.index).expect("应取到附件");
        assert_eq!(name, "report.pdf");
        assert_eq!(mime, "application/pdf");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn converts_html_to_text() {
        let text = html_to_text("<p>Hello&nbsp;<b>world</b></p><style>.a{}</style><div>line2</div>");
        assert!(text.contains("Hello world"));
        assert!(text.contains("line2"));
        assert!(!text.contains(".a"));
    }

    #[test]
    fn falls_back_to_subject_thread() {
        let raw = b"From: a@b.com\r\nSubject: =?UTF-8?B?5L2g5aW9?=\r\n\r\nbody";
        let parsed = parse(raw, &context()).expect("应可解析");
        assert!(parsed.header.thread_id.starts_with("s:"));
        assert_eq!(parsed.header.thread_id.len(), 18);
    }
}
