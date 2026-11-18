use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, LitStr, Result, Token, parse::Parse, parse_macro_input};

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

struct ApiArguments {
    module: LitStr,
    id: LitStr,
    method: LitStr,
    path: LitStr,
    summary: LitStr,
    description: LitStr,
}

impl Parse for ApiArguments {
    fn parse(input: syn::parse::ParseStream<'_>) -> Result<Self> {
        let mut module = None;
        let mut id = None;
        let mut method = None;
        let mut path = None;
        let mut summary = None;
        let mut description = None;

        while !input.is_empty() {
            let name: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: LitStr = input.parse()?;
            match name.to_string().as_str() {
                "module" => module = Some(value),
                "id" => id = Some(value),
                "method" => method = Some(value),
                "path" => path = Some(value),
                "summary" => summary = Some(value),
                "description" => description = Some(value),
                _ => return Err(syn::Error::new(name.span(), "未知的 loki_api 参数")),
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
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

#[proc_macro_attribute]
pub fn loki_api(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ApiArguments);
    let function = parse_macro_input!(item as ItemFn);
    let function_name = &function.sig.ident;
    let registration_name = format_ident!("__LOKI_API_{}", function_name);
    let ApiArguments {
        module,
        id,
        method,
        path,
        summary,
        description,
    } = args;
    let path_value = path.value();
    let path_parameters = path_value
        .split('{')
        .skip(1)
        .filter_map(|part| part.split('}').next())
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

    quote! {
        #function

        #[::linkme::distributed_slice(::loki_services::modules::discover::registry::API_DESCRIPTORS)]
        #[allow(non_upper_case_globals)]
        static #registration_name: ::loki_services::modules::discover::registry::ApiDescriptor =
            ::loki_services::modules::discover::registry::ApiDescriptor {
                module_id: #module,
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
