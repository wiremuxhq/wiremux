//! Field-wise merge of same-id catalog layers.

use std::collections::BTreeMap;

use super::parse::{
    RawBetasField, RawBetasTable, RawFingerprint, RawOauth, RawProfile, RawTokenResponse,
};
use super::types::ListMerge;

/// Later `Some` replaces. Later `None` keeps earlier.
pub(crate) fn merge(earlier: RawProfile, later: RawProfile) -> RawProfile {
    let (e_values, e_header, e_merge) = betas_parts(earlier.betas);
    let (l_values, l_header, l_merge) = betas_parts(later.betas);
    let beta_merge = l_merge.or(e_merge);
    let values = merge_list(e_values, l_values, beta_merge.unwrap_or_default());
    let header = l_header.or(e_header);
    let root_list_merge = later.list_merge.or(earlier.list_merge);
    let header_merge = later.header_merge.or(earlier.header_merge);

    RawProfile {
        schema_version: max_version(earlier.schema_version, later.schema_version),
        id: later.id.or(earlier.id),
        display_name: later.display_name.or(earlier.display_name),
        wire: later.wire.or(earlier.wire),
        stream_events: merge_list(
            earlier.stream_events,
            later.stream_events,
            root_list_merge.unwrap_or_default(),
        ),
        list_merge: root_list_merge,
        tool_type_policy: later.tool_type_policy.or(earlier.tool_type_policy),
        stream_unknown_policy: later
            .stream_unknown_policy
            .or(earlier.stream_unknown_policy),
        base_url: later.base_url.or(earlier.base_url),
        chat_path: later.chat_path.or(earlier.chat_path),
        auth_scheme: later.auth_scheme.or(earlier.auth_scheme),
        headers: merge_map(
            earlier.headers,
            later.headers,
            header_merge.unwrap_or_default(),
        ),
        header_merge,
        aws_service: later.aws_service.or(earlier.aws_service),
        aws_region: later.aws_region.or(earlier.aws_region),
        access_env: later.access_env.or(earlier.access_env),
        oauth: merge_oauth(earlier.oauth, later.oauth),
        fingerprint: merge_fingerprint(earlier.fingerprint, later.fingerprint),
        betas: rebuild_betas(values, header, beta_merge),
        beta_header: later.beta_header.or(earlier.beta_header),
        beta_merge: later.beta_merge.or(earlier.beta_merge),
    }
}

fn max_version(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) | (None, Some(x)) => Some(x),
        (Some(x), Some(y)) => Some(x.max(y)),
    }
}

fn merge_list(
    earlier: Option<Vec<String>>,
    later: Option<Vec<String>>,
    policy: ListMerge,
) -> Option<Vec<String>> {
    match (earlier, later) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(mut a), Some(b)) => match policy {
            ListMerge::Union => {
                for item in b {
                    if !a.contains(&item) {
                        a.push(item);
                    }
                }
                Some(a)
            }
            ListMerge::Replace => Some(b),
        },
    }
}

fn merge_map(
    earlier: Option<BTreeMap<String, String>>,
    later: Option<BTreeMap<String, String>>,
    policy: ListMerge,
) -> Option<BTreeMap<String, String>> {
    match (earlier, later) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(mut a), Some(b)) => match policy {
            ListMerge::Union => {
                a.extend(b);
                Some(a)
            }
            ListMerge::Replace => Some(b),
        },
    }
}

fn merge_oauth(earlier: Option<RawOauth>, later: Option<RawOauth>) -> Option<RawOauth> {
    match (earlier, later) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(a), Some(b)) => {
            let list_merge = b.list_merge.or(a.list_merge);
            Some(RawOauth {
                token_url: b.token_url.or(a.token_url),
                token_url_fallback: b.token_url_fallback.or(a.token_url_fallback),
                authorize_url: b.authorize_url.or(a.authorize_url),
                authorize_params: b.authorize_params.or(a.authorize_params),
                device_auth_url: b.device_auth_url.or(a.device_auth_url),
                client_id: b.client_id.or(a.client_id),
                redirect_uri: b.redirect_uri.or(a.redirect_uri),
                scopes: merge_list(a.scopes, b.scopes, list_merge.unwrap_or_default()),
                list_merge,
                pkce: b.pkce.or(a.pkce),
                refresh_grant: b.refresh_grant.or(a.refresh_grant),
                refresh_body: b.refresh_body.or(a.refresh_body),
                token_request_format: b.token_request_format.or(a.token_request_format),
                token_headers: b.token_headers.or(a.token_headers),
                creds_path: b.creds_path.or(a.creds_path),
                creds_format: b.creds_format.or(a.creds_format),
                access_token_ptr: b.access_token_ptr.or(a.access_token_ptr),
                refresh_token_ptr: b.refresh_token_ptr.or(a.refresh_token_ptr),
                expires_ptr: b.expires_ptr.or(a.expires_ptr),
                expires_unit: b.expires_unit.or(a.expires_unit),
                access_env: b.access_env.or(a.access_env),
                login: b.login.or(a.login),
                setup_token_hint: b.setup_token_hint.or(a.setup_token_hint),
                keychain_service: b.keychain_service.or(a.keychain_service),
                keychain_accounts: b.keychain_accounts.or(a.keychain_accounts),
                token_response: merge_token_response(a.token_response, b.token_response),
            })
        }
    }
}

fn merge_token_response(
    earlier: Option<RawTokenResponse>,
    later: Option<RawTokenResponse>,
) -> Option<RawTokenResponse> {
    match (earlier, later) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(a), Some(b)) => Some(RawTokenResponse {
            access_token_ptr: b.access_token_ptr.or(a.access_token_ptr),
            refresh_token_ptr: b.refresh_token_ptr.or(a.refresh_token_ptr),
            expires_ptr: b.expires_ptr.or(a.expires_ptr),
            expires_unit: b.expires_unit.or(a.expires_unit),
        }),
    }
}

fn merge_fingerprint(
    earlier: Option<RawFingerprint>,
    later: Option<RawFingerprint>,
) -> Option<RawFingerprint> {
    match (earlier, later) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(a), Some(b)) => Some(RawFingerprint {
            user_agent: b.user_agent.or(a.user_agent),
            x_app: b.x_app.or(a.x_app),
            system_prompt_prefix: b.system_prompt_prefix.or(a.system_prompt_prefix),
            tool_name_case: b.tool_name_case.or(a.tool_name_case),
            forbidden_body_fields: b.forbidden_body_fields.or(a.forbidden_body_fields),
            forbidden_field_policy: b.forbidden_field_policy.or(a.forbidden_field_policy),
            extra_body: b.extra_body.or(a.extra_body),
        }),
    }
}

fn betas_parts(
    field: Option<RawBetasField>,
) -> (Option<Vec<String>>, Option<String>, Option<ListMerge>) {
    match field {
        None => (None, None, None),
        Some(RawBetasField::List(values)) => (Some(values), None, None),
        Some(RawBetasField::Table(table)) => (table.values, table.header, table.merge),
    }
}

fn rebuild_betas(
    values: Option<Vec<String>>,
    header: Option<String>,
    merge: Option<ListMerge>,
) -> Option<RawBetasField> {
    if values.is_none() && header.is_none() && merge.is_none() {
        None
    } else {
        Some(RawBetasField::Table(RawBetasTable {
            values,
            header,
            merge,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_appends_unique_later_items() {
        let out = merge_list(
            Some(vec!["a".into(), "b".into()]),
            Some(vec!["b".into(), "c".into()]),
            ListMerge::Union,
        );
        assert_eq!(out, Some(vec!["a".into(), "b".into(), "c".into()]));
    }

    #[test]
    fn replace_takes_later_list_only() {
        let out = merge_list(
            Some(vec!["a".into()]),
            Some(vec!["z".into()]),
            ListMerge::Replace,
        );
        assert_eq!(out, Some(vec!["z".into()]));
    }

    #[test]
    fn omitted_later_list_keeps_earlier() {
        let out = merge_list(Some(vec!["a".into()]), None, ListMerge::Replace);
        assert_eq!(out, Some(vec!["a".into()]));
    }

    #[test]
    fn schema_version_max() {
        assert_eq!(max_version(Some(0), Some(1)), Some(1));
        assert_eq!(max_version(Some(1), None), Some(1));
        assert_eq!(max_version(None, None), None);
    }
}
