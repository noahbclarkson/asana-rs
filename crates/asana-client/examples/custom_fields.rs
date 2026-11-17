//! Reads a task's custom fields with their values typed, then sets a text, a number, an enum
//! (by option name) and a date field, and clears one.
//!
//! ```text
//! ASANA_ACCESS_TOKEN=...      a personal access token
//! ASANA_TASK_GID=...          a task in a project with the fields below
//! ASANA_TEXT_FIELD_GID=...    a text custom field
//! ASANA_NUMBER_FIELD_GID=...  a number custom field
//! ASANA_ENUM_FIELD_GID=...    a single-select custom field
//! ASANA_DATE_FIELD_GID=...    a date custom field
//! cargo run -p asana-client --example custom_fields
//! ```

use std::env;

use asana_client::openapi::ids::{CustomFieldGid, TaskGid};
use asana_client::{Auth, Client, CustomFieldValue, Require, TaskCustomFields, fields};
use rust_decimal::Decimal;
use time::macros::date;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let task: TaskGid = env::var("ASANA_TASK_GID")?.parse()?;
    let text: CustomFieldGid = env::var("ASANA_TEXT_FIELD_GID")?.parse()?;
    let number: CustomFieldGid = env::var("ASANA_NUMBER_FIELD_GID")?.parse()?;
    let stage: CustomFieldGid = env::var("ASANA_ENUM_FIELD_GID")?.parse()?;
    let due: CustomFieldGid = env::var("ASANA_DATE_FIELD_GID")?.parse()?;
    let client = Client::builder()
        .auth(Auth::personal_access_token(env::var("ASANA_ACCESS_TOKEN")?))
        .build()?;

    // `custom_fields` brings every value member; `TaskCustomFields` reads them by the field's
    // kind, so a number is a `Decimal`, an enum an option, a date a date.
    let read = client
        .tasks()
        .get(task.clone())
        .fields(fields![Task: name, custom_fields])
        .send()
        .await?;
    for field in read.custom_fields_typed() {
        match &field.value {
            CustomFieldValue::Number(amount) => println!("{}: the number {amount}", field.gid),
            CustomFieldValue::Enum(option) => {
                println!("{}: the option {:?}", field.gid, option.name);
            }
            CustomFieldValue::Empty => println!("{}: not set", field.gid),
            // Every kind has a variant; `display_value` is Asana's own text for any of them.
            _ => println!("{}: {:?}", field.gid, field.display_value),
        }
    }
    if let Some(current) = read.custom_field(&number).and_then(|f| f.value.as_number()) {
        println!("the number field holds {current}");
    }

    // An enum is set by option GID. Find the option by name, creating it if the field has none.
    let approved = client
        .custom_fields()
        .ensure_option(&stage, "Approved")
        .await?
        .gid
        .require("gid")?;

    // Each value is written in the JSON shape Asana takes for its kind.
    client
        .tasks()
        .update(task.clone())
        .custom_field(text, CustomFieldValue::text("Acme Ltd"))
        .custom_field(number, CustomFieldValue::number(Decimal::new(125_050, 2))) // sent as 1250.5
        .custom_field(stage, CustomFieldValue::enum_option(approved))
        .custom_field(due.clone(), CustomFieldValue::date(date!(2026 - 12 - 17)))
        .send()
        .await?;

    // `clear()` sends `null`, which empties a field of any kind.
    client
        .tasks()
        .update(task)
        .custom_field(due, CustomFieldValue::clear())
        .send()
        .await?;
    Ok(())
}
