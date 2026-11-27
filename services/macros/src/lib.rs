use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, LitStr, Result, Token, Type, parse::Parse, parse_macro_input};

/// 将无参数配置元数据函数注册到编译期配置目录。
#[proc_macro_attribute]
pub fn loki_config(attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    if !attr.is_empty()
        || !function.sig.inputs.is_empty()
        || function.sig.asyncness.is_some()
        || !function.sig.generics.params.is_empty()
    {
        return syn::Error::new_spanned(
            &function.sig,
            "loki_config 仅支持无参数、无泛型的同步元数据函数",
        )
        .to_compile_error()
        .into();
    }
    let name = &function.sig.ident;
    let registration = format_ident!("__LOKI_CONFIG_{}", name);
    quote! {
        #function

        #[::linkme::distributed_slice(::loki_services::modules::config::registry::CONFIG_MODULES)]
        #[allow(non_upper_case_globals)]
        static #registration: ::loki_services::modules::config::registry::ConfigModuleRegistration =
            ::loki_services::modules::config::registry::ConfigModuleRegistration { metadata: #name };
    }.into()
}

/// 将无参数状态元数据函数注册到编译期状态目录。
#[proc_macro_attribute]
pub fn loki_status(attr: TokenStream, item: TokenStream) -> TokenStream {
    let function = parse_macro_input!(item as ItemFn);
    if !attr.is_empty()
        || !function.sig.inputs.is_empty()
        || function.sig.asyncness.is_some()
        || !function.sig.generics.params.is_empty()
    {
        return syn::Error::new_spanned(
            &function.sig,
            "loki_status 仅支持无参数、无泛型的同步元数据函数",
        )
        .to_compile_error()
        .into();
    }
    let name = &function.sig.ident;
    let registration = format_ident!("__LOKI_STATUS_{}", name);
    quote! {
        #function

        #[::linkme::distributed_slice(::loki_services::modules::status::registry::STATUS_MODULES)]
        #[allow(non_upper_case_globals)]
        static #registration: ::loki_services::modules::status::registry::StatusModuleRegistration =
            ::loki_services::modules::status::registry::StatusModuleRegistration { metadata: #name };
    }.into()
}

struct ApiArguments {
    access: LitStr,
    sse_event: Option<Type>,
    module: LitStr,
    id: LitStr,
    method: LitStr,
    path: LitStr,
    summary: LitStr,
    description: LitStr,
    mount: LitStr,
    mount_group: Option<LitStr>,
}

impl Parse for ApiArguments {
    fn parse(input: syn::parse::ParseStream<'_>) -> Result<Self> {
        let mut module = None;
        let mut id = None;
        let mut method = None;
        let mut path = None;
        let mut summary = None;
        let mut description = None;
        let mut access = None;
        let mut sse_event = None;
        let mut mount = None;
        let mut mount_group = None;

        while !input.is_empty() {
            let name: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let key = name.to_string();
            if key == "sse_event" {
                sse_event = Some(input.parse::<Type>()?);
                if input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }
            let value: LitStr = input.parse()?;
            match key.as_str() {
                "module" => module = Some(value),
                "id" => id = Some(value),
                "method" => method = Some(value),
                "path" => path = Some(value),
                "summary" => summary = Some(value),
                "description" => description = Some(value),
                "mount" => mount = Some(value),
                "mount_group" => mount_group = Some(value),
                "access" => {
                    if !matches!(value.value().as_str(), "local" | "paired") {
                        return Err(syn::Error::new(
                            value.span(),
                            "access 只能为 local 或 paired",
                        ));
                    }
                    access = Some(value);
                }
                _ => return Err(syn::Error::new(name.span(), "未知的 loki_api 参数")),
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let access = access.unwrap_or_else(|| LitStr::new("local", input.span()));
        Ok(Self {
            mount: mount.ok_or_else(|| {
                syn::Error::new(input.span(), "缺少 mount，无状态接口使用 mount = \"()\"")
            })?,
            mount_group,
            access,
            sse_event,
            module: module.ok_or_else(|| syn::Error::new(input.span(), "缺少 module"))?,
            id: id.ok_or_else(|| syn::Error::new(input.span(), "缺少 id"))?,
            method: method.ok_or_else(|| syn::Error::new(input.span(), "缺少 method"))?,
            path: path.ok_or_else(|| syn::Error::new(input.span(), "缺少 path"))?,
            summary: summary.ok_or_else(|| syn::Error::new(input.span(), "缺少 summary"))?,
            description: description
                .ok_or_else(|| syn::Error::new(input.span(), "缺少 description"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ApiArguments;

    #[test]
    fn api_metadata_requires_a_mount_declaration() {
        let arguments = r#"
            module = "test",
            id = "test",
            method = "GET",
            path = "/test",
            summary = "测试接口",
            description = "验证接口必须声明挂载状态"
        "#;
        let Err(error) = syn::parse_str::<ApiArguments>(arguments) else {
            panic!("仅声明接口元数据时应拒绝解析");
        };
        assert!(error.to_string().contains("缺少 mount"));

        let mounted = format!(r#"{arguments}, mount = "()""#);
        assert_eq!(
            syn::parse_str::<ApiArguments>(&mounted)
                .unwrap()
                .mount
                .value(),
            "()"
        );
    }
}

#[proc_macro_attribute]
pub fn loki_api(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ApiArguments);
    let function = parse_macro_input!(item as ItemFn);
    let function_name = &function.sig.ident;
    let registration_name = format_ident!("__LOKI_API_{}", function_name);
    let ApiArguments {
        access,
        sse_event,
        module,
        id,
        method,
        path,
        summary,
        description,
        mount,
        mount_group,
    } = args;
    let sse_event = match sse_event {
        Some(event) => quote!(Some(::loki_services::http::sse::metadata::<#event>)),
        None => quote!(None),
    };
    let path_value = path.value();
    let path_parameters = path_value
        .split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
        .map(|name| name.strip_prefix('*').unwrap_or(name))
        .filter(|name| !name.is_empty())
        .map(|name| {
            let name = name.to_owned();
            quote! {
                ::loki_services::modules::discover::registry::ParameterDescriptor {
                    name: #name,
                    location: "path",
                    type_name: "string",
                    required: true,
                    description: "路径参数",
                }
            }
        });

    let state: syn::Type = match mount.parse() {
        Ok(state) => state,
        Err(error) => return error.to_compile_error().into(),
    };
    let method_name = method.value().to_ascii_lowercase();
    if !matches!(
        method_name.as_str(),
        "get" | "post" | "put" | "patch" | "delete" | "head" | "options" | "any"
    ) {
        return syn::Error::new(method.span(), "不支持自动挂载此 HTTP 方法")
            .to_compile_error()
            .into();
    }
    let method_name = format_ident!("{method_name}");
    let group = mount_group.unwrap_or_else(|| module.clone());
    let route_registration = format_ident!("__LOKI_ROUTE_{}", function_name);
    let route = quote! {
        #[::linkme::distributed_slice(::loki_services::http::ROUTES)]
        #[allow(non_upper_case_globals)]
        static #route_registration: ::loki_services::http::RouteRegistration =
            ::loki_services::http::RouteRegistration {
                group: #group,
                path: #path,
                mount: |router| {
                    let router = router.downcast_mut::<::axum::Router<#state>>()
                        .expect(concat!("路由组的状态类型不匹配：", #group, " ", #path));
                    *router = ::std::mem::take(router).route(
                        #path, ::axum::routing::#method_name(#function_name)
                    );
                },
            };
    };

    quote! {
        #function
        #route

        #[::linkme::distributed_slice(::loki_services::modules::discover::registry::API_DESCRIPTORS)]
        #[allow(non_upper_case_globals)]
        static #registration_name: ::loki_services::modules::discover::registry::ApiDescriptor =
            ::loki_services::modules::discover::registry::ApiDescriptor {
                module_id: #module,
                access: #access,
                sse_event: #sse_event,
                id: #id,
                method: #method,
                path: #path,
                summary: #summary,
                description: #description,
                parameters: &[#(#path_parameters),*],
            };
    }
    .into()
}
