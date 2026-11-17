//! Envelopes, empty answers, and decode errors that name a path but never a value.

use asana_client::{DecodeErrorKind, Error};

use crate::support::{
    AddTag, DeleteThing, GetEvents, Scripted, client, get_task, reply, task_body,
};

#[tokio::test(start_paused = true)]
async fn the_data_envelope_is_removed_and_extra_fields_are_ignored() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            200,
            r#"{"data":{"gid":"1201234567890123","name":"Acme launch","unheard_of":{"x":1}},"extra":true}"#,
        ))
    });
    let task = client(transport).send(get_task()).await.unwrap();
    assert_eq!(task.name.as_deref(), Some("Acme launch"));
    assert_eq!(task.gid.unwrap().to_string(), "1201234567890123");
}

#[tokio::test(start_paused = true)]
async fn a_whole_body_answer_keeps_the_siblings_of_data() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            200,
            r#"{"data":[{"action":"changed"}],"sync":"edfc0896b370b7a479886d316131bf5c","has_more":true}"#,
        ))
    });
    let page = client(transport)
        .send(GetEvents { query: Vec::new() })
        .await
        .unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.sync, "edfc0896b370b7a479886d316131bf5c");
    assert!(page.has_more);
}

#[tokio::test(start_paused = true)]
async fn an_empty_answer_decodes_to_an_empty_output() {
    for body in [r#"{"data":{}}"#, "", "{}"] {
        let transport = Scripted::new(move |_, _| Ok(reply(200, body)));
        client(transport.clone()).send(AddTag).await.unwrap();
        client(transport).send(DeleteThing).await.unwrap();
    }
    let transport = Scripted::new(|_, _| Ok(reply(204, "")));
    client(transport).send(DeleteThing).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_decode_error_names_the_path_and_never_the_value() {
    let transport = Scripted::new(|_, _| {
        Ok(reply(
            200,
            r#"{"data":{"gid":"1201234567890123","name":{"payroll":"Acme salaries"}}}"#,
        ))
    });
    let error = client(transport).send(get_task()).await.unwrap_err();
    let Error::Decode {
        operation,
        ref path,
        ref source,
    } = error
    else {
        panic!("{error:?}");
    };
    assert_eq!(operation, "getTask");
    assert_eq!(path, "data.name");
    assert_eq!(source.kind(), &DecodeErrorKind::Data);
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains("Acme salaries"), "{shown}");
        assert!(!shown.contains("payroll"), "{shown}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_missing_envelope_and_a_broken_body_are_decode_errors() {
    let transport = Scripted::new(|_, _| Ok(reply(200, r#"{"gid":"1"}"#)));
    let error = client(transport).send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::Decode { ref source, .. } if source.kind() == &DecodeErrorKind::MissingField("data".into())),
        "{error:?}"
    );

    let transport = Scripted::new(|_, _| Ok(reply(200, task_body().split_at(30).0)));
    let error = client(transport).send(get_task()).await.unwrap_err();
    assert!(
        matches!(error, Error::Decode { ref source, .. } if source.kind() == &DecodeErrorKind::Eof),
        "{error:?}"
    );
}
