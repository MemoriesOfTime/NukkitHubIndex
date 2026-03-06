mod image;
mod link;

use crate::github::{Contributor, GitTreeEntry, Release, Repository, client};
use crate::plugin::{
    Author, Dependency, GalleryItem, License, Links, Plugin, Version, VersionFile,
};
use tracing::debug;


fn parse_timestamp(iso_string: &str) -> u64 {
    use chrono::{DateTime, Utc};
    DateTime::parse_from_rfc3339(iso_string)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).timestamp() as u64)
        .unwrap_or(0)
}


const CATEGORIES: &[&str] = &[
    "adventure",
    "cursed",
    "decoration",
    "economy",
    "equipment",
    "food",
    "game-mechanics",
    "library",
    "magic",
    "management",
    "minigame",
    "mobs",
    "optimization",
    "social",
    "storage",
    "technology",
    "transportation",
    "utility",
    "world-generation",
];

pub struct PostProcessContext<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub branch: &'a str,
}

type ImageProcessorFn = fn(&str, &PostProcessContext, &mut Vec<GalleryItem>) -> String;
type LinkProcessorFn = fn(&str, &PostProcessContext) -> String;

static IMAGE_PROCESSORS: &[ImageProcessorFn] =
    &[image::process_html_images, image::process_md_images];
static LINK_PROCESSORS: &[LinkProcessorFn] = &[link::process_md_links, link::process_html_links];

fn process_readme(readme: &str, ctx: &PostProcessContext) -> (String, Vec<GalleryItem>) {
    let mut content = readme.to_string();
    let mut gallery = Vec::new();

    for processor in IMAGE_PROCESSORS {
        content = processor(&content, ctx, &mut gallery);
    }

    for processor in LINK_PROCESSORS {
        content = processor(&content, ctx);
    }

    (content, gallery)
}

fn get_tree(owner: &str, repo: &str, branch: &str) -> Vec<GitTreeEntry> {
    client()
        .get_tree(owner, repo, branch)
        .map(|t| t.tree)
        .unwrap_or_default()
}

fn find_logo_url(
    tree: &[GitTreeEntry],
    owner: &str,
    repo: &str,
    branch: &str,
) -> Option<String> {
    let logo_paths = [
        ".github/img/logo.png",
        ".github/img/icon.png",
        "logo.png",
        "icon.png",
    ];

    for path in &logo_paths {
        if tree.iter().any(|e| e.path == *path) {
            return Some(format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                owner, repo, branch, path
            ));
        }
    }
    None
}

fn find_gallery_items(
    tree: &[GitTreeEntry],
    owner: &str,
    repo: &str,
    branch: &str,
) -> Vec<GalleryItem> {
    let gallery_dir = ".github/img/";
    let excluded = ["logo.png", "icon.png"];

    tree.iter()
        .filter(|e| {
            e.entry_type == "blob"
                && e.path.starts_with(gallery_dir)
                && e.path.ends_with(".png")
                && !excluded.iter().any(|ex| e.path.ends_with(ex))
        })
        .map(|e| GalleryItem {
            url: format!(
                "https://raw.githubusercontent.com/{}/{}/{}/{}",
                owner, repo, branch, e.path
            ),
            title: String::new(),
            description: String::new(),
            created: String::new(),
        })
        .collect()
}

pub fn build_plugins_from_nukkit(repo: &Repository, plugin_yml_path: &str) -> Vec<Plugin> {
    build_plugins_from_nukkit_with_tree(repo, plugin_yml_path, None)
}

pub fn build_plugins_from_nukkit_with_tree(repo: &Repository, plugin_yml_path: &str, prefetched_tree: Option<Vec<GitTreeEntry>>) -> Vec<Plugin> {
    let (owner, repo_name) = match repo.full_name.split_once('/') {
        Some((o, r)) => (o, r),
        None => {
            debug!(repo = %repo.full_name, "Skip: invalid repo name");
            return Vec::new();
        }
    };

    let default_branch = repo.default_branch.as_deref().unwrap_or("main");

    let yml_content = match client().get_file_content(owner, repo_name, plugin_yml_path) {
        Ok(content) => content,
        Err(e) => {
            debug!(repo = %repo.full_name, error = %e, "Failed to read plugin.yml");
            return Vec::new();
        }
    };

    let nukkit_yml = match crate::nukkit::NukkitPluginYml::from_str(&yml_content) {
        Ok(yml) => yml,
        Err(e) => {
            debug!(repo = %repo.full_name, error = %e, "Failed to parse plugin.yml");
            return Vec::new();
        }
    };

    let releases = client().get_releases(owner, repo_name).unwrap_or_default();
    let readme = client().get_readme(owner, repo_name).unwrap_or_default();
    let contributors = client()
        .get_contributors_by_url(&repo.contributors_url)
        .unwrap_or_default();

    let license = repo.license.as_ref().map_or_else(
        || License {
            id: "ARR".to_string(),
            name: "All Rights Reserved".to_string(),
            url: String::new(),
        },
        |l| {
            let spdx_id = &l.spdx_id;
            let is_valid_spdx = !spdx_id.is_empty()
                && spdx_id != "NOASSERTION"
                && !spdx_id.starts_with("LicenseRef");

            let url = if let Some(html_url) = &l.html_url {
                html_url.clone()
            } else if is_valid_spdx {
                format!("https://spdx.org/licenses/{}.html", spdx_id)
            } else {
                format!("{}/blob/{}/LICENSE", repo.html_url, default_branch)
            };

            License {
                id: spdx_id.clone(),
                name: l.name.clone(),
                url,
            }
        },
    );

    let tree = prefetched_tree.unwrap_or_else(|| get_tree(owner, repo_name, default_branch));
    let icon_url = find_logo_url(&tree, owner, repo_name, default_branch)
        .unwrap_or_else(|| repo.owner.avatar_url.clone());
    let repo_gallery = find_gallery_items(&tree, owner, repo_name, default_branch);
    
    match nukkit_yml_to_plugin(
        nukkit_yml,
        repo,
        &releases,
        &readme,
        &license,
        &contributors,
        owner,
        repo_name,
        default_branch,
        &icon_url,
        repo_gallery,
    ) {
        Some(plugin) => vec![plugin],
        None => Vec::new(),
    }
}

fn nukkit_yml_to_plugin(
    yml: crate::nukkit::NukkitPluginYml,
    repo: &Repository,
    releases: &[Release],
    readme: &str,
    license: &License,
    _contributors: &[Contributor],
    owner: &str,
    repo_name: &str,
    branch: &str,
    icon_url: &str,
    repo_gallery: Vec<GalleryItem>,
) -> Option<Plugin> {
    let ctx = PostProcessContext {
        owner,
        repo: repo_name,
        branch,
    };
    
    let (processed_readme, mut gallery) = process_readme(readme, &ctx);
    gallery.extend(repo_gallery);
    
    let authors = yml.authors.as_vec().into_iter()
        .map(|name| Author {
            name,
            url: String::new(),
            avatar_url: String::new(),
        })
        .collect();
    
    let mut all_dependencies: Vec<Dependency> = yml.depend.iter()
        .map(|name| Dependency {
            plugin_id: name.clone(),
            version_range: String::new(),
            dependency_type: "required".to_string(),
        })
        .collect();
    
    all_dependencies.extend(yml.softdepend.iter().map(|name| Dependency {
        plugin_id: name.clone(),
        version_range: String::new(),
        dependency_type: "optional".to_string(),
    }));
    
    // Build versions from releases
    let versions: Vec<Version> = releases.iter()
        .map(|release| {
            let files: Vec<VersionFile> = release.assets.iter()
                .filter(|a| a.name.ends_with(".jar"))
                .map(|a| VersionFile {
                    filename: a.name.clone(),
                    url: a.browser_download_url.clone(),
                    size: a.size,
                    primary: true,
                })
                .collect();
            
            Version {
                version: release.tag_name.clone(),
                name: release.name.clone().unwrap_or_else(|| release.tag_name.clone()),
                prerelease: release.prerelease,
                changelog: release.body.clone().unwrap_or_default(),
                files,
                downloads: 0,
                published_at: parse_timestamp(&release.published_at.clone().unwrap_or_default()),
            }
        })
        .collect();
    
    let api_version = yml.api.primary().unwrap_or_default();
    
    let categories: Vec<String> = repo.topics.iter()
        .filter_map(|t| {
            // Strip "nukkit-" prefix if present
            let normalized = t.strip_prefix("nukkit-").unwrap_or(t);
            if CATEGORIES.contains(&normalized) {
                Some(normalized.to_string())
            } else {
                None
            }
        })
        .collect();
    
    Some(Plugin {
        id: format!("{}/{}", owner, repo_name),
        name: yml.name,
        source: repo.html_url.clone(),
        summary: yml.description.clone().unwrap_or_default(),
        description: processed_readme,
        authors,
        categories,
        license: license.clone(),
        links: yml.website.as_ref().map(|w| Links {
            homepage: w.clone(),
            wiki: String::new(),
            discord: String::new(),
        }),
        downloads: 0,
        stars: repo.stargazers_count,
        created_at: parse_timestamp(&repo.created_at),
        updated_at: parse_timestamp(&repo.updated_at),
        icon_url: icon_url.to_string(),
        gallery,
        versions,
        api_version,
        server_version: String::new(),
        dependencies: all_dependencies,
        preserved_fields: Default::default(),
    })
}

pub fn parse_github_url(url: &str) -> Option<(String, String)> {
    let url = url.trim_end_matches('/');
    
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }
    
    None
}
