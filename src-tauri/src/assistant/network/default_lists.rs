//! The allow and deny lists a new or migrated agent starts with.
//!
//! Each agent stores its own copy, so a later change here reaches only agents
//! created or migrated after it.

/// Developer hosts allowed by default: code hosting, package registries,
/// model providers, cloud APIs and documentation. No whole-provider
/// wildcard such as `*.amazonaws.com`: those cover storage and API gateways
/// anyone can create, so other cloud hosts are approved one by one.
pub const DEFAULT_ALLOW_DOMAINS: &[&str] = &[
    // Code hosting
    "github.com",
    "*.github.com",
    "*.githubusercontent.com",
    "ghcr.io",
    "gitlab.com",
    "*.gitlab.com",
    "bitbucket.org",
    "*.bitbucket.org",
    "codeberg.org",
    // Rust
    "crates.io",
    "*.crates.io",
    "static.rust-lang.org",
    "sh.rustup.rs",
    "docs.rs",
    // JavaScript
    "registry.npmjs.org",
    "*.npmjs.org",
    "npmjs.com",
    "*.npmjs.com",
    "registry.yarnpkg.com",
    "repo.yarnpkg.com",
    "nodejs.org",
    "jsr.io",
    "deno.land",
    "*.deno.land",
    "bun.sh",
    "unpkg.com",
    "cdn.jsdelivr.net",
    // Python
    "pypi.org",
    "*.pypi.org",
    "files.pythonhosted.org",
    "python.org",
    "*.python.org",
    "conda.anaconda.org",
    "repo.anaconda.com",
    "astral.sh",
    // Go
    "proxy.golang.org",
    "sum.golang.org",
    "golang.org",
    "*.golang.org",
    "go.dev",
    "*.go.dev",
    "gopkg.in",
    // JVM
    "repo.maven.apache.org",
    "repo1.maven.org",
    "*.maven.org",
    "plugins.gradle.org",
    "services.gradle.org",
    "downloads.gradle.org",
    // Other ecosystems
    "rubygems.org",
    "*.rubygems.org",
    "packagist.org",
    "*.packagist.org",
    "api.nuget.org",
    "*.nuget.org",
    "hex.pm",
    "*.hex.pm",
    "pub.dev",
    "hackage.haskell.org",
    "cocoapods.org",
    "*.cocoapods.org",
    "formulae.brew.sh",
    // Containers
    "docker.io",
    "*.docker.io",
    "*.docker.com",
    "quay.io",
    "*.quay.io",
    "mcr.microsoft.com",
    "public.ecr.aws",
    "gcr.io",
    "*.pkg.dev",
    // Model providers
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
    "api.mistral.ai",
    "api.groq.com",
    "api.deepseek.com",
    "api.x.ai",
    "api.cohere.com",
    "api.together.xyz",
    "openrouter.ai",
    "huggingface.co",
    "*.huggingface.co",
    "*.hf.co",
    "ollama.com",
    "*.ollama.com",
    // Cloud APIs
    "management.azure.com",
    "login.microsoftonline.com",
    "graph.microsoft.com",
    "api.cloudflare.com",
    "api.digitalocean.com",
    "api.fly.io",
    "api.vercel.com",
    "api.netlify.com",
    "api.heroku.com",
    // Docs
    "developer.mozilla.org",
    "stackoverflow.com",
    "*.stackexchange.com",
    "*.readthedocs.io",
    "*.readthedocs.org",
    "learn.microsoft.com",
    "developer.apple.com",
    "kubernetes.io",
    "*.wikipedia.org",
    "man7.org",
];

/// Cloud hosts that accept uploads or serve anyone's endpoint, refused even
/// when a user adds a broad entry such as `*.amazonaws.com` to the allow
/// list. Checked before the allow list.
pub const DEFAULT_DENY_DOMAINS: &[&str] = &[
    // S3, global and per bucket
    "s3.amazonaws.com",
    "*.s3.amazonaws.com",
    // S3, regional: s3.<region>.amazonaws.com and <bucket>.s3.<region>.amazonaws.com
    "s3.*.amazonaws.com",
    "*.s3.*.amazonaws.com",
    // S3, dual-stack and Transfer Acceleration
    "s3.dualstack.*.amazonaws.com",
    "*.s3.dualstack.*.amazonaws.com",
    "s3-accelerate.amazonaws.com",
    "*.s3-accelerate.amazonaws.com",
    "s3-accelerate.dualstack.amazonaws.com",
    "*.s3-accelerate.dualstack.amazonaws.com",
    // API Gateway: any account's API, so any receiver
    "*.execute-api.*.amazonaws.com",
    // Google Cloud Storage
    "storage.googleapis.com",
    "*.storage.googleapis.com",
];
