/// Knowledge base indexing: Background task to create plaintext index of .yggdra/knowledge/
/// Supports configurable size limits and battery-aware rate adaptation
use std::path::Path;
use std::fs;
use std::collections::BTreeMap;
use std::thread;
use std::time::Duration;
use crate::battery;

/// Knowledge indexing configuration
#[derive(Debug, Clone)]
pub struct KnowledgeIndexConfig {
    /// Size limit in bytes (default 2GB)
    pub size_limit_bytes: u64,
    /// Base delay between files on battery (milliseconds)
    pub battery_delay_ms: u64,
    /// Whether indexing is enabled
    pub enabled: bool,
}

impl Default for KnowledgeIndexConfig {
    fn default() -> Self {
        Self {
            size_limit_bytes: 20 * 1024 * 1024, // 20MB default (reasonable, not surprising)
            battery_delay_ms: 100,
            enabled: true,
        }
    }
}

/// Category info for the index
#[derive(Debug, Clone)]
struct CategoryInfo {
    path: String,
    file_count: usize,
    indexed_count: usize,
    keywords: Vec<String>,
    files: Vec<(String, Vec<String>)>, // (filename, keywords)
}

/// Index builder state
struct IndexBuilder {
    config: KnowledgeIndexConfig,
    categories: BTreeMap<String, CategoryInfo>,
    total_size: u64,
    indexed_count: u64,
}

impl IndexBuilder {
    fn new(config: KnowledgeIndexConfig) -> Self {
        Self {
            config,
            categories: BTreeMap::new(),
            total_size: 0,
            indexed_count: 0,
        }
    }

    /// Extract keywords from a path string (split by hyphens, convert to keywords)
    fn extract_keywords(path: &str) -> Vec<String> {
        path.split('-')
            .filter(|s| !s.is_empty() && s.len() > 2)
            .map(|s| s.to_lowercase())
            .collect()
    }

    /// Build index from knowledge base directory
    fn build(&mut self, knowledge_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if !knowledge_dir.exists() {
            return Err("Knowledge directory does not exist".into());
        }

        // Iterate through top-level categories
        for entry in fs::read_dir(knowledge_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Skip non-directories and hidden directories
            if !path.is_dir() || path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with('.')) {
                continue;
            }

            let category_name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            self.index_category(&category_name, &path)?;

            // Check size limit
            if self.total_size >= self.config.size_limit_bytes {
                crate::dlog!("🌱 Knowledge index reached size limit ({:.1}GB)", 
                    self.total_size as f64 / 1024.0 / 1024.0 / 1024.0);
                break;
            }

            // Battery awareness: pause on battery
            if battery::is_on_battery() {
                thread::sleep(Duration::from_millis(self.config.battery_delay_ms));
            }
        }

        Ok(())
    }

    /// Index a single category
    fn index_category(&mut self, category: &str, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let mut file_count = 0;
        let mut indexed_count = 0;
        let mut category_keywords = Self::extract_keywords(category);
        let mut files = Vec::new();

        // Count and sample files in category
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_path = entry.path();

            if file_path.is_file() {
                file_count += 1;
                let file_name = file_path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
                    .to_string();

                // Extract keywords from filename
                let file_keywords = Self::extract_keywords(&file_name);
                
                // Track category keywords
                for kw in &file_keywords {
                    if !category_keywords.contains(kw) && category_keywords.len() < 20 {
                        category_keywords.push(kw.clone());
                    }
                }

                // Record files (limit to first 100 per category for index size)
                if indexed_count < 100 {
                    files.push((file_name, file_keywords));
                    indexed_count += 1;

                    // Approximate size increase per file reference (~50 bytes)
                    self.total_size += 50;
                }
            }
        }

        if file_count > 0 {
            self.categories.insert(
                category.to_string(),
                CategoryInfo {
                    path: category.to_string(),
                    file_count,
                    indexed_count,
                    keywords: category_keywords,
                    files,
                },
            );
            self.indexed_count += 1;
        }

        Ok(())
    }

    /// Generate plaintext index
    fn generate_plaintext(&self, total_categories: usize) -> String {
        let mut output = String::new();
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");

        output.push_str(&format!("# Knowledge Base Index\n"));
        output.push_str(&format!("**Version:** 1.0\n"));
        output.push_str(&format!("**Generated:** {}\n", timestamp));
        output.push_str(&format!("**Indexed Size:** {:.2}GB / {:.2}GB\n", 
            self.total_size as f64 / 1024.0 / 1024.0 / 1024.0,
            self.config.size_limit_bytes as f64 / 1024.0 / 1024.0 / 1024.0));
        output.push_str(&format!("**Coverage:** {}/{} categories\n\n", 
            self.indexed_count, total_categories));

        // Indexed categories
        for (category, info) in &self.categories {
            output.push_str(&format!("### {}\n", category));
            output.push_str(&format!("**Files:** {}/{}\n", info.indexed_count, info.file_count));
            output.push_str(&format!("**Path:** .yggdra/knowledge/{}/\n", info.path));
            output.push_str(&format!("**Keywords:** {}\n", info.keywords.join(", ")));

            if info.file_count > info.indexed_count {
                output.push_str(&format!("**Note:** Category partially indexed ({} of {} files shown)\n", 
                    info.indexed_count, info.file_count));
            }

            if !info.files.is_empty() {
                output.push_str("**Sample files:**\n");
                for (filename, _keywords) in info.files.iter().take(5) {
                    output.push_str(&format!("  - {}\n", filename));
                }
            }
            output.push_str("\n");
        }

        // Unindexed categories notice
        let unindexed = total_categories - self.indexed_count as usize;
        if unindexed > 0 {
            output.push_str("---\n\n");
            output.push_str(&format!("### ⚠️ Unindexed Categories ({} remain)\n\n", unindexed));
            output.push_str("For categories not listed above, use ripgrep directly:\n");
            output.push_str("```\n");
            output.push_str("rg \"pattern\" .yggdra/knowledge/category-name/\n");
            output.push_str("```\n\n");
            output.push_str("The agent is aware of this limitation and will suggest ripgrep for exhaustive searches.\n");
        }

        output
    }
}

/// Start background knowledge indexing task
pub fn start_indexing_task(config: Option<KnowledgeIndexConfig>) {
    let config = config.unwrap_or_default();

    if !config.enabled {
        crate::dlog!("🌱 Knowledge indexing disabled");
        return;
    }

    // Spawn background thread for indexing
    std::thread::spawn(move || {
        if let Err(e) = index_knowledge_base(&config) {
            crate::dlog!("🌱 Knowledge indexing error: {}", e);
        }
    });
}

/// Main indexing task (runs in background thread)
fn index_knowledge_base(config: &KnowledgeIndexConfig) -> Result<(), Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let knowledge_dir = cwd.join(".yggdra").join("knowledge");

    crate::dlog!("🌱 Starting knowledge base indexing (size limit: {:.1}GB)", 
        config.size_limit_bytes as f64 / 1024.0 / 1024.0 / 1024.0);

    // Count total categories first
    let mut total_categories = 0;
    for entry in fs::read_dir(&knowledge_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && !path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with('.')) {
            total_categories += 1;
        }
    }

    // Build index
    let mut builder = IndexBuilder::new(config.clone());
    builder.build(&knowledge_dir)?;

    // Generate and write index
    let index_content = builder.generate_plaintext(total_categories);
    let index_path = cwd.join(".yggdra").join("knowledge").join("INDEX.md");

    fs::write(&index_path, &index_content)?;
    crate::dlog!("🌱 Knowledge index complete: {} categories indexed ({:.2}GB)", 
        builder.indexed_count,
        builder.total_size as f64 / 1024.0 / 1024.0 / 1024.0);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords() {
        let keywords = IndexBuilder::extract_keywords("rust-async-programming");
        assert!(keywords.contains(&"rust".to_string()));
        assert!(keywords.contains(&"async".to_string()));
        assert!(keywords.contains(&"programming".to_string()));
    }

    #[test]
    fn test_index_builder_creation() {
        let config = KnowledgeIndexConfig::default();
        let builder = IndexBuilder::new(config);
        assert_eq!(builder.total_size, 0);
        assert_eq!(builder.indexed_count, 0);
    }

    #[test]
    fn test_config_defaults() {
        let config = KnowledgeIndexConfig::default();
        assert_eq!(config.size_limit_bytes, 20 * 1024 * 1024); // 20MB default
        assert_eq!(config.battery_delay_ms, 100);
        assert!(config.enabled);
    }
}
