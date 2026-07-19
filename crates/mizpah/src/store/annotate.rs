//! In-memory bookmarks / tags / comments keyed by log entry id.

use super::Store;
use crate::filter::compile_query;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    #[serde(default)]
    pub marked: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotatedEntry {
    pub id: u64,
    pub annotation: Annotation,
}

impl Store {
    pub async fn set_bookmark(
        &self,
        id: u64,
        marked: Option<bool>,
        tags: Option<Vec<String>>,
        comment: Option<Option<String>>,
    ) -> Option<Annotation> {
        let result = {
            let mut inner = self.inner.write().await;
            if !inner.entries.iter().any(|e| e.id == id) {
                return None;
            }
            let ann = inner.annotations.entry(id).or_default();
            if let Some(m) = marked {
                ann.marked = m;
            }
            if let Some(t) = tags {
                ann.tags = t;
            }
            if let Some(c) = comment {
                ann.comment = c;
            }
            // Drop empty annotations
            if !ann.marked && ann.tags.is_empty() && ann.comment.as_ref().is_none_or(|s| s.is_empty())
            {
                let removed = inner.annotations.remove(&id).unwrap_or_default();
                Some(removed)
            } else {
                Some(ann.clone())
            }
        };
        if let Some(ref ann) = result {
            self.persist_annotation(id, ann).await;
        }
        result
    }

    pub async fn list_bookmarks(&self) -> Vec<AnnotatedEntry> {
        let inner = self.inner.read().await;
        let mut out: Vec<AnnotatedEntry> = inner
            .annotations
            .iter()
            .filter(|(_, a)| a.marked || !a.tags.is_empty() || a.comment.is_some())
            .map(|(&id, annotation)| AnnotatedEntry {
                id,
                annotation: annotation.clone(),
            })
            .collect();
        out.sort_by_key(|e| e.id);
        out
    }

    /// Tag all entries matching a CEL filter. Returns number of entries tagged.
    pub async fn tag_by_cel(&self, cel: &str, tag: &str) -> Result<usize, String> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Err("tag is required".into());
        }
        let query = compile_query(cel).map_err(|e| e.to_string())?;
        let updated: Vec<(u64, Annotation)> = {
            let mut inner = self.inner.write().await;
            let ids: Vec<u64> = inner
                .entries
                .iter()
                .filter(|e| crate::filter::matches_entry(&e.service, &e.data, &query))
                .map(|e| e.id)
                .collect();
            let mut updated = Vec::with_capacity(ids.len());
            for id in ids {
                let ann = inner.annotations.entry(id).or_default();
                if !ann.tags.iter().any(|t| t == tag) {
                    ann.tags.push(tag.to_string());
                }
                updated.push((id, ann.clone()));
            }
            updated
        };
        let n = updated.len();
        for (id, ann) in updated {
            self.persist_annotation(id, &ann).await;
        }
        Ok(n)
    }

    #[cfg(test)]
    pub async fn get_annotation(&self, id: u64) -> Option<Annotation> {
        let inner = self.inner.read().await;
        inner.annotations.get(&id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use crate::store::Store;

    #[tokio::test]
    async fn bookmark_and_tag_by_cel() {
        let store = Store::new(1_000_000);
        let e = store
            .push_line("api", r#"{"level":"error","msg":"boom"}"#)
            .await;
        let id = e[0].id;
        store
            .set_bookmark(id, Some(true), Some(vec!["keep".into()]), Some(Some("note".into())))
            .await
            .unwrap();
        let list = store.list_bookmarks().await;
        assert_eq!(list.len(), 1);
        assert!(list[0].annotation.marked);

        let n = store.tag_by_cel(r#"level == "error""#, "err").await.unwrap();
        assert_eq!(n, 1);
        let ann = store.get_annotation(id).await.unwrap();
        assert!(ann.tags.contains(&"err".into()));
    }

    #[tokio::test]
    async fn set_bookmark_missing_id_returns_none() {
        let store = Store::new(1_000_000);
        assert!(
            store
                .set_bookmark(999, Some(true), None, None)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn clearing_annotation_removes_record() {
        let store = Store::new(1_000_000);
        let e = store.push_line("api", r#"{"level":"info"}"#).await;
        let id = e[0].id;
        store
            .set_bookmark(id, Some(true), None, None)
            .await
            .unwrap();
        store
            .set_bookmark(id, Some(false), Some(vec![]), Some(Some("".into())))
            .await
            .unwrap();
        assert!(store.get_annotation(id).await.is_none());
        assert!(store.list_bookmarks().await.is_empty());
    }

    #[tokio::test]
    async fn tag_by_cel_rejects_empty_tag() {
        let store = Store::new(1_000_000);
        store
            .push_line("api", r#"{"level":"error"}"#)
            .await;
        let err = store.tag_by_cel(r#"level == "error""#, "  ").await;
        assert_eq!(err.unwrap_err(), "tag is required");
    }

    #[tokio::test]
    async fn partial_update_persists_when_annotation_remains() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(1_000_000);
        store.enable_persist(dir.path()).await.unwrap();
        let e = store.push_line("api", r#"{"level":"info"}"#).await;
        let id = e[0].id;
        store
            .set_bookmark(id, Some(true), None, None)
            .await
            .unwrap();
        store
            .set_bookmark(id, None, Some(vec!["tag".into()]), None)
            .await
            .unwrap();
        let ann = store.get_annotation(id).await.unwrap();
        assert!(ann.marked);
        assert_eq!(ann.tags, vec!["tag".to_string()]);
    }
}
