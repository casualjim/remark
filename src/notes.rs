use anyhow::{Context, Result};
use gix::Repository;
use gix::bstr::ByteSlice;
use gix::object::tree::EntryKind;
use gix::objs;
use gix_hash::ObjectId;
use gix_ref::transaction::PreviousValue;
use smallvec::SmallVec;

fn note_path(oid: &ObjectId) -> String {
    let hex = oid.to_string();
    let (a, b) = hex.split_at(2.min(hex.len()));
    format!("{a}/{b}")
}

pub fn read(repo: &Repository, notes_ref: &str, target: &ObjectId) -> Result<Option<String>> {
    let Some(r) = repo
        .try_find_reference(notes_ref)
        .context("find notes ref")?
    else {
        return Ok(None);
    };
    let commit = r
        .id()
        .object()
        .context("read notes ref object")?
        .peel_to_commit()
        .context("notes ref is not a commit")?;
    let tree = commit.tree().context("notes tree")?;

    let p = note_path(target);
    let Some(entry) = tree.lookup_entry_by_path(&p).context("lookup note entry")? else {
        return Ok(None);
    };
    let blob = entry
        .object()
        .context("load note blob")?
        .try_into_blob()
        .context("note entry is not a blob")?;
    Ok(Some(
        String::from_utf8_lossy(blob.data.as_ref()).to_string(),
    ))
}

pub fn write(
    repo: &Repository,
    notes_ref: &str,
    target: &ObjectId,
    note: Option<&str>,
) -> Result<()> {
    let mut previous_commit: Option<ObjectId> = None;
    let mut root_tree = repo.empty_tree();

    if let Some(r) = repo
        .try_find_reference(notes_ref)
        .context("find notes ref")?
    {
        let id = r.id();
        previous_commit = Some(id.detach());
        let commit = id
            .object()
            .context("read previous notes commit")?
            .peel_to_commit()
            .context("notes ref is not a commit")?;
        root_tree = commit.tree().context("previous notes tree")?;
    }

    let mut editor =
        gix::object::tree::Editor::new(&root_tree).context("init notes tree editor")?;
    let p = note_path(target);

    match note {
        Some(text) if !text.trim().is_empty() => {
            let blob_id = repo
                .write_blob(text.as_bytes())
                .context("write note blob")?
                .detach();
            editor
                .upsert(p.as_bytes().as_bstr(), EntryKind::Blob, blob_id)
                .context("upsert note path")?;
        }
        _ => {
            editor
                .remove(p.as_bytes().as_bstr())
                .context("remove note")?;
        }
    };

    let tree_id = editor.write().context("write notes tree")?.detach();

    // Write commit object directly (no config dependency), then update the notes ref.
    let sig = default_signature();
    let mut parents: SmallVec<[ObjectId; 1]> = Default::default();
    if let Some(p) = previous_commit {
        parents.push(p);
    }
    let commit = objs::Commit {
        tree: tree_id,
        parents,
        author: sig.clone().into(),
        committer: sig.into(),
        encoding: None,
        message: "git-review: update notes\n".into(),
        extra_headers: Default::default(),
    };
    let commit_id = repo
        .write_object(commit)
        .context("write notes commit")?
        .detach();

    repo.reference(
        notes_ref,
        commit_id,
        PreviousValue::Any,
        "git-review: update notes",
    )
    .context("update notes ref")?;

    Ok(())
}

fn default_signature() -> gix_actor::Signature {
    use gix_date::Time;
    let name = std::env::var("GIT_AUTHOR_NAME")
        .or_else(|_| std::env::var("GIT_COMMITTER_NAME"))
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "git-review".to_string());
    let email = std::env::var("GIT_AUTHOR_EMAIL")
        .or_else(|_| std::env::var("GIT_COMMITTER_EMAIL"))
        .unwrap_or_else(|_| "git-review@localhost".to_string());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    gix_actor::Signature {
        name: name.into(),
        email: email.into(),
        time: Time {
            seconds: now,
            offset: 0,
        },
    }
}
