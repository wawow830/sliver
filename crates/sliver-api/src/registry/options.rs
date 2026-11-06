//! What an app asks for when it creates a context (`CreateContext` options).

use super::*;

#[derive(Debug, Clone)]
pub struct Target {
    pub app_id: Option<String>,
    pub workspace: Option<String>,
    pub title: Option<regex_lite::Regex>,
}

#[derive(Debug, Clone)]
pub enum Scope {
    App,
    Target(Target),
    Status,
}

impl Scope {
    pub fn name(&self) -> &'static str {
        match self {
            Scope::App => "app",
            Scope::Target(_) => "target",
            Scope::Status => "status",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub scope: Scope,
    pub provider: Option<String>,
    pub title: Option<String>,
    pub window: Option<String>,
    pub include_children: bool,
    pub modal: bool,
}

pub fn parse_options(p: &Props, w: &mut Warnings) -> Result<ContextOptions, ApiError> {
    let st = |k: &str| match p.get(k) {
        Some(Prop::Str(s)) => Ok(Some(s.clone())),
        None => Ok(None),
        Some(_) => Err(ApiError::InvalidArgs(format!(
            "option {k:?} must be a string"
        ))),
    };
    let flag = |k: &str| match p.get(k) {
        Some(Prop::Bool(b)) => Ok(*b),
        None => Ok(false),
        Some(_) => Err(ApiError::InvalidArgs(format!(
            "option {k:?} must be a boolean"
        ))),
    };
    let scope = match st("scope")?.as_deref() {
        None | Some("app") => Scope::App,
        Some("status") => Scope::Status,
        Some("target") => {
            let Some(Prop::Map(t)) = p.get("target") else {
                return Err(ApiError::InvalidArgs(
                    "scope \"target\" needs a \"target\" a{sv} selector".into(),
                ));
            };
            let get = |k: &str| {
                t.get(k).and_then(|v| {
                    if let Prop::Str(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
            };
            let title = match get("title-regex") {
                Some(r) if r.len() <= 256 => Some(
                    regex_lite::Regex::new(&r)
                        .map_err(|e| ApiError::InvalidArgs(format!("title-regex: {e}")))?,
                ),
                Some(_) => return Err(ApiError::InvalidArgs("title-regex too long".into())),
                None => None,
            };
            let target = Target {
                app_id: get("app-id"),
                workspace: get("workspace"),
                title,
            };
            if target.app_id.is_none() && target.workspace.is_none() && target.title.is_none() {
                return Err(ApiError::InvalidArgs("empty target selector".into()));
            }
            Scope::Target(target)
        }
        Some(other) => return Err(ApiError::InvalidArgs(format!("unknown scope {other:?}"))),
    };
    for k in p.keys() {
        if ![
            "scope",
            "provider",
            "title",
            "window",
            "target",
            "include-children",
            "modal",
        ]
        .contains(&k.as_str())
        {
            w.push(format!("unknown option {k:?} ignored"));
        }
    }
    let provider = st("provider")?;
    if let Some(pr) = &provider
        && (pr.is_empty()
            || pr.len() > 255
            || pr.contains('/')
            || pr.chars().any(char::is_whitespace))
    {
        return Err(ApiError::InvalidArgs(
            "provider must be a reverse-DNS id".into(),
        ));
    }
    let opts = ContextOptions {
        provider,
        title: st("title")?.map(|t| t.chars().filter(|c| !c.is_control()).take(64).collect()),
        window: st("window")?,
        include_children: flag("include-children")?,
        modal: flag("modal")?,
        scope,
    };
    if !matches!(opts.scope, Scope::App)
        && (opts.modal || opts.include_children || opts.window.is_some())
    {
        w.push("modal/include-children/window only apply to app scope, ignored".into());
    }
    Ok(opts)
}
