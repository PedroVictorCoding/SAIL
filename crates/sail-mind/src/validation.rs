use base64::{Engine, engine::general_purpose::STANDARD};
use sail_protocol::*;
pub fn validate_request(request: &InferenceRequest, limits: &ResourceLimits) -> Result<usize> {
    if request.version != VERSION {
        return Err(InferenceError::new(
            ErrorCode::UnsupportedVersion,
            "Only inference protocol version 1 is supported",
        ));
    }
    if request.messages.is_empty()
        || request.messages.len() > 128
        || !request.messages.iter().any(|m| m.role == Role::User)
        || request.generation.max_tokens == 0
        || request.generation.max_tokens > limits.max_output_tokens
        || !request.generation.temperature.is_finite()
        || !(0.0..=2.0).contains(&request.generation.temperature)
        || request.tools.len() > 8
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidRequest,
            "Invalid messages, generation parameters or tool count",
        ));
    }
    let mut text_bytes = 0usize;
    let mut images = 0usize;
    let mut outstanding = std::collections::HashSet::new();
    for message in &request.messages {
        match &message.content {
            Content::Text(text) => text_bytes = text_bytes.saturating_add(text.len()),
            Content::Parts(parts) => {
                for part in parts {
                    match part {
                        ContentPart::Text { text } => {
                            text_bytes = text_bytes.saturating_add(text.len())
                        }
                        ContentPart::ImageUrl { image_url } => {
                            if message.role != Role::User {
                                return Err(InferenceError::new(
                                    ErrorCode::InvalidRequest,
                                    "Images must be supplied in user messages",
                                ));
                            }
                            images += 1;
                            if images > limits.max_images {
                                return Err(InferenceError::new(
                                    ErrorCode::ResourceLimit,
                                    "Too many images",
                                ));
                            }
                            validate_image(image_url, limits)?;
                        }
                    }
                }
            }
        }
        if message.role == Role::Tool {
            if !message
                .tool_call_id
                .as_ref()
                .is_some_and(|id| outstanding.remove(id))
            {
                return Err(InferenceError::new(
                    ErrorCode::InvalidRequest,
                    "Tool result does not match an outstanding assistant tool call",
                ));
            }
        } else if message.tool_call_id.is_some() {
            return Err(InferenceError::new(
                ErrorCode::InvalidRequest,
                "tool_call_id is only valid for tool results",
            ));
        }
        if !message.tool_calls.is_empty() && message.role != Role::Assistant {
            return Err(InferenceError::new(
                ErrorCode::InvalidRequest,
                "Only assistant messages may propose tool calls",
            ));
        }
        for call in &message.tool_calls {
            if call.id.is_empty()
                || call.kind != "function"
                || call.function.name.is_empty()
                || !outstanding.insert(call.id.clone())
            {
                return Err(InferenceError::new(
                    ErrorCode::InvalidRequest,
                    "Malformed or duplicate assistant tool call",
                ));
            }
            text_bytes =
                text_bytes.saturating_add(call.function.arguments.len() + call.function.name.len());
        }
    }
    if !outstanding.is_empty() {
        return Err(InferenceError::new(
            ErrorCode::InvalidRequest,
            "Assistant tool calls require matching observations before inference continues",
        ));
    }
    for tool in &request.tools {
        if tool.kind != "function"
            || tool.function.name.is_empty()
            || !tool.function.parameters.is_object()
        {
            return Err(InferenceError::new(
                ErrorCode::InvalidRequest,
                "Invalid tool definition",
            ));
        }
        text_bytes = text_bytes.saturating_add(
            serde_json::to_vec(tool)
                .map_err(|e| InferenceError::new(ErrorCode::InvalidRequest, e.to_string()))?
                .len(),
        );
    }
    if images > limits.max_images || text_bytes > limits.max_input_bytes {
        return Err(InferenceError::new(
            ErrorCode::ResourceLimit,
            "Input text or image count exceeds configured budget",
        ));
    }
    Ok(images)
}
pub fn validate_limits(limits: &ResourceLimits) -> Result<()> {
    if !(1..=4).contains(&limits.max_parallel)
        || limits.max_queued > 64
        || limits.max_input_bytes == 0
        || limits.max_response_bytes == 0
        || limits.max_images > 4
        || limits.max_image_bytes == 0
        || limits.max_output_tokens == 0
        || limits.queue_timeout_ms == 0
        || limits.inference_timeout_ms == 0
    {
        return Err(InferenceError::new(
            ErrorCode::InvalidConfiguration,
            "Invalid inference resource limits",
        ));
    }
    Ok(())
}

fn validate_image(image: &ImageUrl, limits: &ResourceLimits) -> Result<()> {
    let data = image
        .url
        .strip_prefix("data:image/png;base64,")
        .or_else(|| image.url.strip_prefix("data:image/jpeg;base64,"))
        .ok_or_else(|| {
            InferenceError::new(
                ErrorCode::ForbiddenEndpoint,
                "Images must be inline PNG/JPEG data; remote image fetches are forbidden",
            )
        })?;
    if data.len() > limits.max_image_bytes.saturating_mul(4) / 3 + 8 {
        return Err(InferenceError::new(
            ErrorCode::ResourceLimit,
            "Image exceeds configured byte budget",
        ));
    }
    let decoded = STANDARD
        .decode(data)
        .map_err(|_| InferenceError::new(ErrorCode::InvalidRequest, "Invalid image base64"))?;
    if decoded.is_empty() || decoded.len() > limits.max_image_bytes {
        return Err(InferenceError::new(
            ErrorCode::ResourceLimit,
            "Image is empty or too large",
        ));
    }
    let format = if image.url.starts_with("data:image/png;") {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Jpeg
    };
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(decoded), format)
        .into_dimensions()
        .map_err(|e| {
            InferenceError::new(
                ErrorCode::InvalidRequest,
                format!("Invalid encoded image: {e}"),
            )
        })?;
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(InferenceError::new(
            ErrorCode::ResourceLimit,
            "Image dimensions exceed 4096x4096",
        ));
    }
    Ok(())
}
