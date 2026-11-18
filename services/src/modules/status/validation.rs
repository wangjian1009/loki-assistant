use serde_json::Value;

use super::model::StatusSchema;
use super::service::StatusError;

/// 校验外部值，只返回字段路径和原因，不在错误中回显提交值。
pub fn validate(schema: &StatusSchema, value: &Value, path: &str) -> Result<(), StatusError> {
    let invalid = |reason: &str| StatusError::InvalidValue {
        path: path.to_owned(),
        reason: reason.to_owned(),
    };
    match schema {
        StatusSchema::Any => Ok(()),
        StatusSchema::String { choices } => {
            if !value.is_string() {
                return Err(invalid("必须是 string"));
            }
            if !choices.is_empty() && !choices.iter().any(|choice| choice.value == *value) {
                return Err(invalid("不在允许的枚举值中"));
            }
            Ok(())
        }
        StatusSchema::Integer => {
            if value.as_i64().is_some() || value.as_u64().is_some() {
                Ok(())
            } else {
                Err(invalid("必须是 integer"))
            }
        }
        StatusSchema::Number => {
            if value.is_number() {
                Ok(())
            } else {
                Err(invalid("必须是 number"))
            }
        }
        StatusSchema::Boolean => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(invalid("必须是 boolean"))
            }
        }
        StatusSchema::Null => {
            if value.is_null() {
                Ok(())
            } else {
                Err(invalid("必须是 null"))
            }
        }
        StatusSchema::Array { items } => {
            let array = value.as_array().ok_or_else(|| invalid("必须是 array"))?;
            for (index, value) in array.iter().enumerate() {
                validate(items, value, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        StatusSchema::Object { fields } => {
            let object = value.as_object().ok_or_else(|| invalid("必须是 object"))?;
            for field in fields {
                let field_path = format!("{path}.{}", field.key);
                match object.get(&field.key) {
                    None if field.required => {
                        return Err(StatusError::InvalidValue {
                            path: field_path,
                            reason: "缺少必填字段".to_owned(),
                        });
                    }
                    None => {}
                    Some(value) if field.nullable && value.is_null() => {}
                    Some(value) => validate(&field.schema, value, &field_path)?,
                }
            }
            for key in object.keys() {
                if !fields.iter().any(|field| &field.key == key) {
                    return Err(StatusError::InvalidValue {
                        path: format!("{path}.{key}"),
                        reason: "未声明的字段".to_owned(),
                    });
                }
            }
            Ok(())
        }
    }
}
