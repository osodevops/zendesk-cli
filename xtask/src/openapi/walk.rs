//! Tolerant OpenAPI 3.0 walker: `serde_yaml_ng::Value` → normalised `serde_json::Value`.
//!
//! Zendesk's documents are not strictly valid OAS (Help Center references an undefined
//! `oauth2` security scheme, `style: deepObject` sits on a `oneOf` parameter, `support.yaml`
//! carries a bare `=` scalar, some mapping keys are numbers), so this walker never validates.
//! It stringifies mapping keys, resolves only the local `$ref`s it needs, and ignores the rest.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Map, Value};

use super::{Method, ParamLocation, ParamType, SpecName};

/// How many `#/components/schemas/*` hops are inlined into the `detail` payload: the referenced
/// schema itself plus the references directly inside it. Deeper references stay as `$ref`.
pub const SCHEMA_INLINE_DEPTH: usize = 2;

/// Composite-schema recursion limit for `items_key` inference (`allOf`/`oneOf`/`anyOf`).
const ARRAY_PROPS_DEPTH: usize = 3;

const METHOD_KEYS: [(&str, Method); 5] = [
    ("get", Method::Get),
    ("post", Method::Post),
    ("put", Method::Put),
    ("patch", Method::Patch),
    ("delete", Method::Delete),
];

/// A parsed spec: metadata, every operation, and the raw document for `$ref` lookups.
#[derive(Debug)]
pub struct Document {
    pub spec: SpecName,
    pub openapi: String,
    pub info_version: String,
    pub path_count: usize,
    pub operations: Vec<RawOperation>,
    root: Value,
}

/// One operation straight from the document, before inference.
#[derive(Debug, Clone)]
pub struct RawOperation {
    pub id: String,
    pub method: Method,
    pub path: String,
    pub tags: Vec<String>,
    pub summary: String,
    pub description: String,
    pub deprecated: bool,
    /// Path-level parameters first, then operation-level ones; same `(name, in)` overrides.
    pub params: Vec<RawParam>,
    pub body: Option<RawBody>,
    /// Status code → schema (`None` for bodiless responses such as 204). Unresolved.
    pub responses: BTreeMap<String, Option<Value>>,
}

#[derive(Debug, Clone)]
pub struct RawParam {
    pub name: String,
    pub location: ParamLocation,
    pub required: bool,
    pub deep_object: bool,
    /// Schema uses `oneOf`/`anyOf` (e.g. Zendesk's `DualPaginationPage`).
    pub one_of: bool,
    pub ty: ParamType,
    pub description: String,
    /// Unresolved schema, as written (may be a `$ref`).
    pub schema: Value,
}

#[derive(Debug, Clone)]
pub struct RawBody {
    pub required: bool,
    pub content_type: String,
    pub schema: Option<Value>,
}

impl Document {
    pub fn parse_file(spec: SpecName, path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(spec, &text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(spec: SpecName, text: &str) -> Result<Self> {
        let yaml: serde_yaml_ng::Value =
            serde_yaml_ng::from_str(text).with_context(|| format!("{spec}: not valid YAML"))?;
        let root = yaml_to_json(yaml);
        let obj = root
            .as_object()
            .ok_or_else(|| anyhow!("{spec}: document root is not a mapping"))?;
        let openapi = string_at(obj, "openapi").unwrap_or_default();
        if !openapi.starts_with("3.") {
            bail!("{spec}: unsupported `openapi: {openapi:?}` (expected 3.x)");
        }
        let info_version = obj
            .get("info")
            .and_then(Value::as_object)
            .and_then(|i| string_at(i, "version"))
            .unwrap_or_default();

        let mut doc = Self {
            spec,
            openapi,
            info_version,
            path_count: 0,
            operations: Vec::new(),
            root,
        };
        let paths = doc
            .root
            .get("paths")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("{spec}: missing `paths` mapping"))?;
        doc.path_count = paths.len();

        let mut operations = Vec::new();
        for (path, item) in paths {
            let Some(item) = item.as_object() else {
                bail!("{spec}: path item {path} is not a mapping");
            };
            let path_params: Vec<Value> = item
                .get("parameters")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for (key, value) in item {
                let Some((_, method)) = METHOD_KEYS.iter().find(|(k, _)| k == key) else {
                    continue; // parameters/summary/description/servers/head/options/trace
                };
                let Some(op) = value.as_object() else {
                    bail!("{spec}: {} {path} is not a mapping", method.as_str());
                };
                let parsed = doc
                    .parse_operation(path, *method, op, &path_params)
                    .with_context(|| format!("{spec}: {} {path}", method.as_str()))?;
                operations.push(parsed);
            }
        }
        doc.operations = operations;
        Ok(doc)
    }

    fn parse_operation(
        &self,
        path: &str,
        method: Method,
        op: &Map<String, Value>,
        path_params: &[Value],
    ) -> Result<RawOperation> {
        let id =
            string_at(op, "operationId").ok_or_else(|| anyhow!("operation has no operationId"))?;
        let tags: Vec<String> = op
            .get("tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let summary = string_at(op, "summary")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let description = string_at(op, "description")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let deprecated = op
            .get("deprecated")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut params: Vec<RawParam> = Vec::new();
        let op_params = op
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for raw in path_params.iter().chain(op_params) {
            let Some(param) = self.parse_param(raw)? else {
                continue;
            };
            match params
                .iter_mut()
                .find(|p| p.name == param.name && p.location == param.location)
            {
                Some(existing) => *existing = param,
                None => params.push(param),
            }
        }

        let body = match op.get("requestBody") {
            Some(rb) => {
                let rb = self.deref(rb)?;
                let rb = rb
                    .as_object()
                    .ok_or_else(|| anyhow!("requestBody is not a mapping"))?;
                let required = rb.get("required").and_then(Value::as_bool).unwrap_or(false);
                let (content_type, schema) = pick_content(rb.get("content"));
                Some(RawBody {
                    required,
                    content_type,
                    schema,
                })
            }
            None => None,
        };

        let mut responses = BTreeMap::new();
        if let Some(resps) = op.get("responses").and_then(Value::as_object) {
            for (code, resp) in resps {
                let resp = self.deref(resp)?;
                let schema = resp
                    .as_object()
                    .and_then(|r| pick_content(r.get("content")).1);
                responses.insert(code.clone(), schema);
            }
        }

        Ok(RawOperation {
            id,
            method,
            path: path.to_owned(),
            tags,
            summary,
            description,
            deprecated,
            params,
            body,
            responses,
        })
    }

    /// `Ok(None)` for parameter locations the CLI never sends (`cookie`).
    fn parse_param(&self, raw: &Value) -> Result<Option<RawParam>> {
        let raw = self.deref(raw)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| anyhow!("parameter is not a mapping"))?;
        let name = string_at(obj, "name").ok_or_else(|| anyhow!("parameter has no name"))?;
        let location = match string_at(obj, "in").as_deref() {
            Some("path") => ParamLocation::Path,
            Some("query") => ParamLocation::Query,
            Some("header") => ParamLocation::Header,
            Some("cookie") => return Ok(None),
            other => bail!("parameter {name}: unsupported `in: {other:?}`"),
        };
        let required = location == ParamLocation::Path
            || obj
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let deep_object = string_at(obj, "style").as_deref() == Some("deepObject");
        let description = string_at(obj, "description")
            .unwrap_or_default()
            .trim()
            .to_owned();
        let schema = obj.get("schema").cloned().unwrap_or(Value::Null);
        let resolved = self.deref(&schema)?;
        let one_of = resolved.get("oneOf").is_some() || resolved.get("anyOf").is_some();
        let ty = match resolved.get("type").and_then(Value::as_str) {
            Some("string") => ParamType::String,
            Some("integer") => ParamType::Integer,
            Some("number") => ParamType::Number,
            Some("boolean") => ParamType::Boolean,
            Some("array") => ParamType::Array,
            Some("object") => ParamType::Object,
            _ if deep_object || resolved.get("properties").is_some() => ParamType::Object,
            _ => ParamType::String,
        };
        Ok(Some(RawParam {
            name,
            location,
            required,
            deep_object,
            one_of,
            ty,
            description,
            schema,
        }))
    }

    /// Resolve a local JSON reference such as `#/components/schemas/Ticket`.
    pub fn resolve(&self, reference: &str) -> Option<&Value> {
        let pointer = reference.strip_prefix('#')?;
        let mut cur = &self.root;
        for token in pointer.split('/').skip(1) {
            let token = token.replace("~1", "/").replace("~0", "~");
            cur = match cur {
                Value::Object(m) => m.get(&token)?,
                Value::Array(a) => a.get(token.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// If `v` is `{ "$ref": "#/..." }` return the referenced value (one hop), else `v` itself.
    fn deref(&self, v: &Value) -> Result<Value> {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => self
                .resolve(r)
                .cloned()
                .ok_or_else(|| anyhow!("unresolvable $ref {r:?}")),
            None => Ok(v.clone()),
        }
    }

    fn deref_ref<'a>(&'a self, v: &'a Value) -> Option<&'a Value> {
        match v.get("$ref").and_then(Value::as_str) {
            Some(r) => self.resolve(r),
            None => Some(v),
        }
    }

    /// Names of the array-typed properties of `schema`, in document order. Looks through one
    /// `$ref` per level and into `allOf`/`oneOf`/`anyOf` parts. A schema that is itself an array
    /// yields nothing (the body *is* the list).
    pub fn array_properties(&self, schema: &Value) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_array_properties(schema, 0, &mut out);
        out
    }

    fn collect_array_properties(&self, schema: &Value, depth: usize, out: &mut Vec<String>) {
        if depth > ARRAY_PROPS_DEPTH {
            return;
        }
        let Some(schema) = self.deref_ref(schema) else {
            return;
        };
        let Some(obj) = schema.as_object() else {
            return;
        };
        if let Some(props) = obj.get("properties").and_then(Value::as_object) {
            for (name, prop) in props {
                let is_array = self
                    .deref_ref(prop)
                    .and_then(|p| p.get("type"))
                    .and_then(Value::as_str)
                    .is_some_and(|t| t == "array");
                if is_array && !out.iter().any(|n| n == name) {
                    out.push(name.clone());
                }
            }
        }
        for key in ["allOf", "oneOf", "anyOf"] {
            if let Some(parts) = obj.get(key).and_then(Value::as_array) {
                for part in parts {
                    self.collect_array_properties(part, depth + 1, out);
                }
            }
        }
    }

    /// Inline `#/components/schemas/*` references up to [`SCHEMA_INLINE_DEPTH`] hops, leaving
    /// deeper or cyclic references as `$ref` objects.
    pub fn inline_schema(&self, schema: &Value) -> Value {
        let mut stack = Vec::new();
        self.inline(schema, 0, &mut stack)
    }

    fn inline(&self, v: &Value, depth: usize, stack: &mut Vec<String>) -> Value {
        match v {
            Value::Object(obj) => {
                if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
                    if depth < SCHEMA_INLINE_DEPTH
                        && !stack.iter().any(|s| s == r)
                        && let Some(target) = self.resolve(r)
                    {
                        stack.push(r.to_owned());
                        let out = self.inline(target, depth + 1, stack);
                        stack.pop();
                        return out;
                    }
                    return v.clone();
                }
                let mut out = Map::new();
                for (k, val) in obj {
                    out.insert(k.clone(), self.inline(val, depth, stack));
                }
                Value::Object(out)
            }
            Value::Array(items) => {
                Value::Array(items.iter().map(|i| self.inline(i, depth, stack)).collect())
            }
            other => other.clone(),
        }
    }
}

/// `(content type, schema)` of a `content` mapping, preferring `application/json`.
fn pick_content(content: Option<&Value>) -> (String, Option<Value>) {
    let Some(content) = content.and_then(Value::as_object) else {
        return (String::new(), None);
    };
    let entry = content
        .get_key_value("application/json")
        .or_else(|| content.iter().next());
    match entry {
        Some((ctype, media)) => (ctype.clone(), media.get("schema").cloned()),
        None => (String::new(), None),
    }
}

fn string_at(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key).map(|v| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// Convert YAML to JSON, stringifying non-string mapping keys and unwrapping tags.
pub fn yaml_to_json(v: serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Y;
    match v {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                n.as_f64().map_or(Value::Null, Value::from)
            }
        }
        Y::String(s) => Value::String(s),
        Y::Sequence(seq) => Value::Array(seq.into_iter().map(yaml_to_json).collect()),
        Y::Mapping(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                out.insert(key_to_string(k), yaml_to_json(v));
            }
            Value::Object(out)
        }
        Y::Tagged(t) => yaml_to_json(t.value),
    }
}

fn key_to_string(k: serde_yaml_ng::Value) -> String {
    use serde_yaml_ng::Value as Y;
    match k {
        Y::String(s) => s,
        Y::Null => "null".to_owned(),
        Y::Bool(b) => b.to_string(),
        Y::Number(n) => n.to_string(),
        other => serde_json::to_string(&yaml_to_json(other)).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = r#"
openapi: 3.0.3
info: { title: t, version: "1.2.3" }
paths:
  /api/v2/things:
    parameters:
      - $ref: '#/components/parameters/Account'
    get:
      operationId: ListThings
      tags: [Things]
      summary: List things
      parameters:
        - $ref: '#/components/parameters/DualPaginationPage'
        - name: per_page
          in: query
          schema: { type: integer }
      responses:
        200:
          content:
            application/json:
              schema: { $ref: '#/components/schemas/ThingsResponse' }
    post:
      operationId: CreateThing
      tags: [Things]
      deprecated: true
      requestBody:
        required: true
        content:
          application/json:
            schema: { $ref: '#/components/schemas/Thing' }
      responses:
        "201":
          content:
            application/json:
              schema: { $ref: '#/components/schemas/Thing' }
        "204": { description: nothing }
  /api/v2/things/{id}:
    delete:
      operationId: DeleteThing
      tags: [Things]
      parameters:
        - name: id
          in: path
          schema: { type: integer }
        - name: session
          in: cookie
          schema: { type: string }
      responses:
        "204": { description: gone }
components:
  parameters:
    Account:
      name: account
      in: header
      schema: { type: string }
    DualPaginationPage:
      name: page
      in: query
      style: deepObject
      schema:
        oneOf:
          - type: integer
          - type: object
  schemas:
    ThingsResponse:
      type: object
      properties:
        count: { type: integer }
        things: { type: array, items: { $ref: '#/components/schemas/Thing' } }
    Thing:
      type: object
      properties:
        id: { type: integer }
        parent: { $ref: '#/components/schemas/Thing' }
        weird:
          example:
            - change: =
              1: numeric key
"#;

    #[test]
    fn walks_a_minimal_document() {
        let doc = Document::parse(SpecName::Support, MINI).expect("parse");
        assert_eq!(doc.openapi, "3.0.3");
        assert_eq!(doc.info_version, "1.2.3");
        assert_eq!(doc.path_count, 2);
        assert_eq!(doc.operations.len(), 3);

        let list = &doc.operations[0];
        assert_eq!(list.id, "ListThings");
        assert_eq!(list.method, Method::Get);
        let names: Vec<(&str, ParamLocation)> = list
            .params
            .iter()
            .map(|p| (p.name.as_str(), p.location))
            .collect();
        assert_eq!(
            names,
            vec![
                ("account", ParamLocation::Header),
                ("page", ParamLocation::Query),
                ("per_page", ParamLocation::Query)
            ]
        );
        let page = &list.params[1];
        assert!(page.deep_object && page.one_of);
        assert_eq!(page.ty, ParamType::Object);
        assert_eq!(list.responses.keys().collect::<Vec<_>>(), vec!["200"]);
        let schema = list.responses["200"].as_ref().expect("schema");
        assert_eq!(doc.array_properties(schema), vec!["things"]);

        let create = &doc.operations[1];
        assert!(create.deprecated);
        let body = create.body.as_ref().expect("body");
        assert!(body.required);
        assert_eq!(body.content_type, "application/json");
        assert!(create.responses["204"].is_none());

        let delete = &doc.operations[2];
        assert_eq!(delete.params.len(), 1, "cookie params are skipped");
        assert!(delete.params[0].required, "path params are always required");
    }

    #[test]
    fn inlining_is_depth_limited_and_cycle_safe() {
        let doc = Document::parse(SpecName::Support, MINI).expect("parse");
        let schema = doc.operations[0].responses["200"].clone().expect("schema");
        let inlined = doc.inline_schema(&schema);
        assert_eq!(inlined["properties"]["things"]["type"], "array");
        // depth 2: Thing inlined under items, its self-reference kept as $ref
        let thing = &inlined["properties"]["things"]["items"];
        assert_eq!(thing["properties"]["id"]["type"], "integer");
        assert_eq!(
            thing["properties"]["parent"]["$ref"],
            "#/components/schemas/Thing"
        );
        // numeric keys and the bare `=` scalar survive normalisation
        assert_eq!(thing["properties"]["weird"]["example"][0]["change"], "=");
        assert_eq!(
            thing["properties"]["weird"]["example"][0]["1"],
            "numeric key"
        );
    }

    #[test]
    fn stringifies_odd_mapping_keys() {
        let v: serde_yaml_ng::Value =
            serde_yaml_ng::from_str("? [1, 2]\n: seq\ntrue: t\nnull: n\n1.5: f\n").expect("yaml");
        let j = yaml_to_json(v);
        let o = j.as_object().expect("object");
        assert_eq!(
            o.keys().cloned().collect::<Vec<_>>(),
            vec!["[1,2]", "true", "null", "1.5"]
        );
    }
}
