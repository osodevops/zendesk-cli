//! Runners shared by every resource command: stream a list through the sink (with sideload
//! joins, `--all`/`--limit`/`--checkpoint`, `--jq`, `-o raw`), collect a whole walk, and send
//! one request with `--dry-run` treated as success.

use std::pin::pin;

use futures_util::StreamExt;
use serde_json::Value;
use zdk_core::api::PageDialect;
use zdk_core::http::{RequestSpec, ZendeskClient};
use zdk_core::output::{self, OutputFormat, sideload};
use zdk_core::pagination::{PageOptions, extract_items, stream_pages};
use zdk_core::{Result, ZdkError};

use crate::context::AppContext;

/// A client-side record filter.
pub(crate) type Keep = Box<dyn Fn(&Value) -> bool + Send>;

/// A list to stream.
pub(crate) struct ListRun {
    pub spec: RequestSpec,
    pub dialect: PageDialect,
    pub items_key: Option<&'static str>,
    /// Table preset name.
    pub preset: Option<&'static str>,
    /// Join sideloaded `users`/`groups`/… onto each record.
    pub join: bool,
    /// Client-side record filter (e.g. `--public-only`).
    pub keep: Option<Keep>,
}

impl ListRun {
    /// A list for `spec`, taking the dialect and items key from its registry operation.
    pub(crate) fn new(spec: RequestSpec, preset: Option<&'static str>) -> Self {
        let mut spec = spec;
        let op = spec.resolve_op();
        Self {
            spec,
            dialect: op.map_or(PageDialect::Cursor, |o| o.pagination),
            items_key: op.and_then(|o| o.items_key),
            preset,
            join: true,
            keep: None,
        }
    }

    pub(crate) fn keep(mut self, f: impl Fn(&Value) -> bool + Send + 'static) -> Self {
        self.keep = Some(Box::new(f));
        self
    }

    /// Override the registry's dialect (endpoints the spec lists without page parameters
    /// but that do paginate, e.g. `ListGroupUsers`).
    pub(crate) fn dialect(mut self, dialect: PageDialect) -> Self {
        self.dialect = dialect;
        self
    }
}

impl std::fmt::Debug for ListRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListRun")
            .field("spec", &self.spec)
            .field("dialect", &self.dialect)
            .field("items_key", &self.items_key)
            .field("preset", &self.preset)
            .field("join", &self.join)
            .finish_non_exhaustive()
    }
}

/// Stream the list to stdout in the active format. `--jq` buffers the whole result; `-o raw`
/// prints each page body untouched; `--dry-run` prints the first request and returns `Ok`.
pub(crate) async fn stream_list(ctx: &AppContext, run: ListRun) -> Result<()> {
    let client = ctx.client().await?;
    let raw = ctx.output == OutputFormat::Raw;
    let keep = run.keep;
    let accept = |v: &Value| keep.as_ref().is_none_or(|f| f(v));

    if run.dialect == PageDialect::None {
        // Not paginated: one request, records extracted the same way.
        let Some(resp) = send_raw(&client, run.spec).await? else {
            return Ok(());
        };
        if raw {
            output::raw::write(&resp.body);
            return Ok(());
        }
        let body = resp.value()?;
        let mut items = extract_items(&body, run.items_key);
        if run.join {
            sideload::embed_page(&mut items, &body);
        }
        items.retain(|v| accept(v));
        return ctx.emit(Value::Array(items), run.preset);
    }

    let opts = PageOptions::from_settings(&ctx.settings);
    let items_key = run.items_key.map(str::to_string);
    let mut pages = pin!(stream_pages(
        client,
        run.spec,
        run.dialect,
        items_key,
        opts,
        ctx.cancel.clone()
    ));

    if ctx.settings.output.jq.is_some() {
        let mut items = Vec::new();
        while let Some(page) = pages.next().await {
            let mut page = match page {
                Ok(p) => p,
                Err(ZdkError::DryRun) => return Ok(()),
                Err(e) => return Err(e),
            };
            if run.join {
                sideload::embed_page(&mut page.items, &page.body);
            }
            items.extend(page.items.into_iter().filter(|v| accept(v)));
        }
        return ctx.emit(Value::Array(items), run.preset);
    }

    let render = ctx.render_options(run.preset);
    let mut sink = output::sink(ctx.output, &render);
    let mut started = false;
    while let Some(page) = pages.next().await {
        let mut page = match page {
            Ok(p) => p,
            Err(ZdkError::DryRun) => return Ok(()),
            Err(e) => return Err(e),
        };
        if raw {
            output::json::write_value(&page.body, true);
            continue;
        }
        if run.join {
            sideload::embed_page(&mut page.items, &page.body);
        }
        if !started {
            sink.begin()?;
            started = true;
        }
        for item in page.items {
            if accept(&item) {
                sink.item(output::project(item, &render)?)?;
            }
        }
    }
    if started {
        sink.end()?;
    }
    Ok(())
}

/// Walk every page of `spec` and return the (joined) records. `None` under `--dry-run`.
pub(crate) async fn collect_all(
    ctx: &AppContext,
    spec: RequestSpec,
    join: bool,
) -> Result<Option<Vec<Value>>> {
    let client = ctx.client().await?;
    let mut spec = spec;
    let op = spec.resolve_op();
    let dialect = op.map_or(PageDialect::Cursor, |o| o.pagination);
    let items_key = op.and_then(|o| o.items_key).map(str::to_string);
    if dialect == PageDialect::None {
        let Some(body) = send(&client, spec).await? else {
            return Ok(None);
        };
        let mut items = extract_items(&body, items_key.as_deref());
        if join {
            sideload::embed_page(&mut items, &body);
        }
        return Ok(Some(items));
    }
    let opts = PageOptions {
        all: true,
        limit: None,
        checkpoint: None,
        ..PageOptions::from_settings(&ctx.settings)
    };
    let mut pages = pin!(stream_pages(
        client,
        spec,
        dialect,
        items_key,
        opts,
        ctx.cancel.clone()
    ));
    let mut items = Vec::new();
    while let Some(page) = pages.next().await {
        let mut page = match page {
            Ok(p) => p,
            Err(ZdkError::DryRun) => return Ok(None),
            Err(e) => return Err(e),
        };
        if join {
            sideload::embed_page(&mut page.items, &page.body);
        }
        items.extend(page.items);
    }
    Ok(Some(items))
}

/// Send one request; `Ok(None)` when `--dry-run` printed it instead.
pub(crate) async fn send(client: &ZendeskClient, spec: RequestSpec) -> Result<Option<Value>> {
    match client.execute(spec).await {
        Ok(resp) => resp.value().map(Some),
        Err(ZdkError::DryRun) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Send one request and hand back the raw response bytes (for `-o raw`); `Ok(None)` on dry-run.
pub(crate) async fn send_raw(
    client: &ZendeskClient,
    spec: RequestSpec,
) -> Result<Option<zdk_core::http::ApiResponse>> {
    match client.execute(spec).await {
        Ok(resp) => Ok(Some(resp)),
        Err(ZdkError::DryRun) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Emit one fetched object: `-o raw` prints the body untouched, otherwise the object under
/// `key` (sideloads joined) goes through projection, `--jq` and the format.
pub(crate) async fn fetch_single(
    ctx: &AppContext,
    spec: RequestSpec,
    key: &str,
    preset: Option<&str>,
) -> Result<()> {
    let client = ctx.client().await?;
    let Some(resp) = send_raw(&client, spec).await? else {
        return Ok(());
    };
    if ctx.output == OutputFormat::Raw {
        output::raw::write(&resp.body);
        return Ok(());
    }
    let body = resp.value()?;
    ctx.emit(sideload::embed_single(body, key), preset)
}

/// Print a count: the bare number in table mode, `{"count": n, …}` otherwise.
pub(crate) fn emit_count(ctx: &AppContext, count: u64, refreshed_at: Option<&str>) -> Result<()> {
    let mut value = serde_json::json!({ "count": count });
    if let Some(at) = refreshed_at
        && let Some(map) = value.as_object_mut()
    {
        map.insert("refreshed_at".into(), Value::String(at.to_string()));
    }
    ctx.emit_or_line(value, &count.to_string())
}
