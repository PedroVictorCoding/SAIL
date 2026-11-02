use serde_json::Value;

pub struct StagedAckCatalog;

impl StagedAckCatalog {
    /// Detect conversational locale from active voice timbre or prompt tokens.
    pub fn detect_locale(prompt: &str, voice: Option<&str>) -> &'static str {
        if let Some(v) = voice {
            match v {
                "pf_dora" | "pm_alex" => return "pt",
                "ef_dora" => return "es",
                "af_heart" | "am_michael" => return "en",
                _ => {}
            }
        }

        let p_lower = prompt.to_lowercase();
        let pt_markers = [
            "olá",
            "tempo",
            "previsão",
            "sistema",
            "verificar",
            "pesquisar",
            "qual",
            "quais",
            "como está",
            "ficheiro",
            "tarefa",
            "obrigado",
            "por favor",
        ];
        for marker in pt_markers {
            if p_lower.contains(marker) {
                return "pt";
            }
        }

        let es_markers = ["hola", "tiempo", "pronóstico", "gracias", "por favor"];
        for marker in es_markers {
            if p_lower.contains(marker) {
                return "es";
            }
        }

        "en"
    }

    /// Retrieve verified acknowledgement phrase for a proposed tool call.
    pub fn phrase_for(tool_name: &str, _params: &Value, locale: &str) -> &'static str {
        let tool = tool_name.to_lowercase();
        match locale {
            "pt" => {
                if tool.contains("weather") {
                    "A verificar a previsão do tempo..."
                } else if tool.contains("forge") {
                    "A verificar as tarefas do Forge..."
                } else if tool.contains("web") || tool.contains("search") || tool.contains("fetch")
                {
                    "A pesquisar na web..."
                } else if tool.contains("status") || tool.contains("os") || tool.contains("system")
                {
                    "A verificar o estado do sistema..."
                } else if tool.contains("file") || tool.contains("read") {
                    "A ler o ficheiro solicitado..."
                } else {
                    "A verificar isso agora..."
                }
            }
            "es" => {
                if tool.contains("weather") {
                    "Consultando el pronóstico del tiempo..."
                } else if tool.contains("forge") {
                    "Consultando las tareas de Forge..."
                } else if tool.contains("web") || tool.contains("search") || tool.contains("fetch")
                {
                    "Buscando en la web..."
                } else if tool.contains("status") || tool.contains("os") || tool.contains("system")
                {
                    "Comprobando el estado del sistema..."
                } else {
                    "Consultando eso ahora..."
                }
            }
            _ => {
                if tool.contains("weather") {
                    "Checking the weather forecast..."
                } else if tool.contains("forge") {
                    "Checking Forge coding tasks..."
                } else if tool.contains("web") || tool.contains("search") || tool.contains("fetch")
                {
                    "Searching the web for that..."
                } else if tool.contains("status") || tool.contains("os") || tool.contains("system")
                {
                    "Checking system status..."
                } else if tool.contains("file") || tool.contains("read") {
                    "Reading the requested file..."
                } else {
                    "Looking into that now..."
                }
            }
        }
    }
}
