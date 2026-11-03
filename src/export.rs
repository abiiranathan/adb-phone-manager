//! Data export routines for contacts, messages and call logs.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::adb::{CallRecord, Contact, Sms, SmsKind};

pub fn export_contacts_vcf(contacts: &[Contact], path: &Path) -> Result<(), std::io::Error> {
    let mut file = fs::File::create(path)?;
    for c in contacts {
        writeln!(file, "BEGIN:VCARD")?;
        writeln!(file, "VERSION:3.0")?;
        writeln!(file, "FN:{}", c.name)?;
        writeln!(file, "TEL:{}", c.number)?;
        writeln!(file, "END:VCARD")?;
    }
    Ok(())
}

pub fn export_contacts_csv(contacts: &[Contact], path: &Path) -> Result<(), std::io::Error> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "\"Name\",\"Phone Number\"")?;
    for c in contacts {
        writeln!(
            file,
            "\"{}\",\"{}\"",
            c.name.replace('"', "\"\""),
            c.number.replace('"', "\"\"")
        )?;
    }
    Ok(())
}

pub fn export_messages_csv(messages: &[Sms], path: &Path) -> Result<(), std::io::Error> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "\"Direction\",\"Number\",\"Timestamp\",\"Body\"")?;
    for m in messages {
        let dir = match m.kind {
            SmsKind::Received => "Incoming",
            SmsKind::Sent => "Outgoing",
            SmsKind::Other => "Other",
        };
        writeln!(
            file,
            "\"{}\",\"{}\",\"{}\",\"{}\"",
            dir,
            m.address.replace('"', "\"\""),
            m.date_ms,
            m.body.replace('"', "\"\"")
        )?;
    }
    Ok(())
}

pub fn export_calls_csv(calls: &[CallRecord], path: &Path) -> Result<(), std::io::Error> {
    let mut file = fs::File::create(path)?;
    writeln!(
        file,
        "\"Type\",\"Name\",\"Number\",\"Timestamp\",\"Duration_Seconds\""
    )?;
    for c in calls {
        writeln!(
            file,
            "\"{}\",\"{}\",\"{}\",\"{}\",\"{}\"",
            c.call_type.label(),
            c.name.replace('"', "\"\""),
            c.number.replace('"', "\"\""),
            c.date_ms,
            c.duration_secs
        )?;
    }
    Ok(())
}
