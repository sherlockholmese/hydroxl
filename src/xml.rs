//! Helpers for editing libvirt XML while preserving unrelated markup.
use anyhow::{Context, Result, ensure};
use std::{io::Write, ops::Range};

pub(crate) fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn temporary(xml: &str) -> Result<tempfile::NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .suffix(".xml")
        .tempfile()
        .context("Could not create a temporary configuration.")?;
    file.write_all(xml.as_bytes()).context("Could not write the temporary configuration.")?;
    Ok(file)
}

/// Returns an edit that inserts markup into an existing element, including an empty one.
pub(crate) fn append_child(
    xml: &str,
    element: roxmltree::Node<'_, '_>,
    child: &str,
) -> Result<(Range<usize>, String)> {
    let range = element.range();
    let text = xml
        .get(range.clone())
        .filter(|text| element.is_element() && text.starts_with('<'))
        .context("Could not locate the XML element in its source text.")?;
    if let Some(open) = text.strip_suffix("/>") {
        let name =
            open[1..].split_whitespace().next().context("Could not identify the XML element.")?;
        Ok((range, format!("{open}>{child}</{name}>")))
    } else {
        let close = text.rfind("</").context("The XML element is missing its closing tag.")?;
        let at = range.start + close;
        Ok((at..at, child.to_owned()))
    }
}

/// All edit ranges refer to the same original document. Reject overlapping edits before changing it.
pub(crate) fn apply_edits(xml: &mut String, mut edits: Vec<(Range<usize>, String)>) -> Result<()> {
    edits.sort_by(|(left, _), (right, _)| {
        right.start.cmp(&left.start).then(right.end.cmp(&left.end))
    });
    let mut end = xml.len();
    for (range, _) in &edits {
        ensure!(
            range.end <= end && xml.get(range.clone()).is_some(),
            "Overlapping or invalid changes in the machine configuration."
        );
        end = range.start;
    }
    for (range, replacement) in edits {
        xml.replace_range(range, &replacement);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_qualified_empty_and_explicit_elements() -> Result<()> {
        for devices in ["<v:devices />", "<v:devices></v:devices >"] {
            let mut xml = format!("<domain xmlns:v='urn:test'>{devices}</domain>");
            let doc = roxmltree::Document::parse(&xml)?;
            let devices = doc.root_element().first_element_child().context("missing devices")?;
            let edit = append_child(&xml, devices, "<disk/>")?;
            apply_edits(&mut xml, vec![edit])?;
            let doc = roxmltree::Document::parse(&xml)?;
            assert!(doc.descendants().any(|n| n.has_tag_name("disk")));
        }
        Ok(())
    }

    #[test]
    fn rejects_overlapping_changes_before_applying_them() {
        let original = "<devices><disk/></devices>";
        let mut xml = original.to_owned();
        assert!(
            apply_edits(&mut xml, vec![(0..26, String::new()), (9..16, String::new())]).is_err()
        );
        assert_eq!(xml, original);
    }
}
