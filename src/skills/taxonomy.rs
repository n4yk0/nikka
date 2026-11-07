use std::sync::LazyLock;

use regex::Regex;

pub struct Skill {
    pub name: &'static str,
    pub category: &'static str,
    matcher: Regex,
}

impl Skill {
    /// `folded` must already be lowercased and accent-free (`textutil::fold`).
    pub fn matches(&self, folded: &str) -> bool {
        self.matcher.is_match(folded)
    }
}

/// Boundaries are explicit because `\b` ignores "+", "#" and "." in tokens like c++, c# or .net.
fn bounded_alternation(patterns: &[&str]) -> Regex {
    Regex::new(&format!("(?:^|[^a-z0-9])(?:{})(?:$|[^a-z0-9])", patterns.join("|"))).expect("valid taxonomy pattern")
}

fn skill(name: &'static str, category: &'static str, patterns: &[&str]) -> Skill {
    Skill { name, category, matcher: bounded_alternation(patterns) }
}

const AI: &str = "AI";
const LANGUAGE: &str = "Language";
const FRONTEND: &str = "Frontend";
const INFRA: &str = "Cloud & Ops";
const DATA: &str = "Data";
const PRACTICE: &str = "Practice";

static TAXONOMY: LazyLock<Vec<Skill>> = LazyLock::new(|| {
    vec![
        skill(
            "AI / LLM engineering",
            AI,
            &[
                "llms?",
                "large language models?",
                "gen ?ai",
                "generative ai",
                "ia generative",
                "intelligence artificielle generative",
                "rag",
                "retrieval[- ]augmented",
                "langchain",
                "langgraph",
                "llamaindex",
                "openai",
                "anthropic",
                "claude",
                "chatgpt",
                "gpt-?[345o][a-z0-9.]*",
                "prompt engineering",
                "ai agents?",
                "agents? (?:ia|ai)",
                "agentic",
                "multi-agents?",
                "mcp",
                "model context protocol",
                "vector (?:db|database|search|store)s?",
                "embeddings?",
                "fine[- ]tun(?:e|ing)",
                "ai engineer(?:ing)?",
            ],
        ),
        skill(
            "Machine learning / MLOps",
            AI,
            &[
                "machine learning",
                "ml",
                "deep learning",
                "pytorch",
                "tensorflow",
                "jax",
                "scikit-learn",
                "sklearn",
                "mlops",
                "apprentissage automatique",
                "computer vision",
                "nlp",
                "data scien(?:ce|tist)s?",
            ],
        ),
        skill("Python", LANGUAGE, &["python", "django", "fastapi", "flask"]),
        skill("TypeScript", LANGUAGE, &["typescript"]),
        skill("JavaScript", LANGUAGE, &["javascript", "ecmascript"]),
        skill(
            "Go",
            LANGUAGE,
            &[
                "golang",
                "go(?:[,/)]| developers?| engineers?| backend| services| microservices)",
                "(?:in|with|using|en) go",
            ],
        ),
        skill("Rust", LANGUAGE, &["rust"]),
        skill("Java / Spring", LANGUAGE, &["java", "spring ?boot", "spring framework", "jvm"]),
        skill("Kotlin", LANGUAGE, &["kotlin"]),
        skill("C# / .NET", LANGUAGE, &["c#", r"\.net", "dotnet", r"asp\.net", "blazor"]),
        skill("C / C++", LANGUAGE, &[r"c\+\+", "cpp"]),
        skill("PHP", LANGUAGE, &["php", "laravel", "symfony"]),
        skill("Ruby / Rails", LANGUAGE, &["ruby", "rails"]),
        skill("Elixir", LANGUAGE, &["elixir", "phoenix framework"]),
        skill("React", FRONTEND, &[r"react(?:\.?js)?", r"next\.?js"]),
        skill("Vue", FRONTEND, &[r"vue(?:\.?js)?", r"nuxt(?:\.?js)?"]),
        skill("Angular", FRONTEND, &["angular(?:js)?"]),
        skill("Node.js", FRONTEND, &[r"node\.?js", "nestjs", r"express\.?js"]),
        skill("Mobile", FRONTEND, &["ios", "android", "react native", "flutter", "swift", "ionic", "capacitor"]),
        skill("AWS", INFRA, &["aws", "amazon web services"]),
        skill("GCP", INFRA, &["gcp", "google cloud"]),
        skill("Azure", INFRA, &["azure"]),
        skill("Kubernetes", INFRA, &["kubernetes", "k8s", "helm", "openshift", "eks", "gke", "aks"]),
        skill("Docker / containers", INFRA, &["docker", "containers?", "podman", "conteneurs?", "conteneurisation"]),
        skill(
            "Terraform / IaC",
            INFRA,
            &["terraform", "opentofu", "pulumi", "ansible", "infrastructure as code", "iac"],
        ),
        skill(
            "CI/CD & DevOps",
            INFRA,
            &["ci ?/ ?cd", "devops", "gitlab(?:[- ]ci)?", "github actions", "jenkins", "argo ?cd"],
        ),
        skill(
            "Observability",
            INFRA,
            &["observabilit(?:y|e)", "prometheus", "grafana", "datadog", "opentelemetry", "otel"],
        ),
        skill("Linux", INFRA, &["linux", "unix"]),
        skill("SQL databases", DATA, &["sql", "postgres(?:ql)?", "mysql", "mariadb", "sql server", "oracle"]),
        skill(
            "NoSQL / search",
            DATA,
            &["nosql", "mongo(?:db)?", "redis", "cassandra", "dynamodb", "elasticsearch", "opensearch"],
        ),
        skill(
            "Data engineering",
            DATA,
            &[
                "spark",
                "kafka",
                "airflow",
                "dbt",
                "snowflake",
                "databricks",
                "bigquery",
                "etl",
                "data pipelines?",
                "data engineer(?:ing)?",
            ],
        ),
        skill(
            "Product engineering",
            PRACTICE,
            &[
                "product engineers?",
                "product engineering",
                "product[- ]minded",
                "product mindset",
                "product sense",
                "product thinking",
                "user research",
                "customer[- ]facing",
                "a/b test(?:s|ing)?",
                "product analytics",
                "end[- ]to[- ]end ownership",
            ],
        ),
        skill("Full-stack", PRACTICE, &["full[- ]?stack"]),
        skill(
            "Distributed systems",
            PRACTICE,
            &[
                "distributed systems?",
                "system design",
                "micro-?services?",
                "event[- ]driven",
                "domain[- ]driven",
                "ddd",
            ],
        ),
        skill(
            "Security / DevSecOps",
            PRACTICE,
            &[
                "devsecops",
                "appsec",
                "application security",
                "owasp",
                "cybersecurit(?:y|e)",
                "secure coding",
                "pentest(?:ing)?",
                "iso ?27001",
                "securite applicative",
            ],
        ),
        skill(
            "Testing / TDD",
            PRACTICE,
            &[
                "tdd",
                "test[- ]driven",
                "unit tests?",
                "tests? unitaires",
                "e2e",
                "cypress",
                "playwright",
                "jest",
                "phpunit",
                "pytest",
            ],
        ),
        skill("Agile / Scrum", PRACTICE, &["agile", "agilite", "scrum", "kanban"]),
        skill("API design", PRACTICE, &["rest ?api", "api rest", "restful", "graphql", "grpc", "openapi"]),
    ]
});

pub fn taxonomy() -> &'static [Skill] {
    &TAXONOMY
}

pub struct Signal {
    pub name: &'static str,
    matcher: Regex,
}

impl Signal {
    pub fn matches(&self, folded: &str) -> bool {
        self.matcher.is_match(folded)
    }
}

static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
    let signal = |name, patterns: &[&str]| Signal { name, matcher: bounded_alternation(patterns) };
    vec![
        signal(
            "AI in the job description",
            &[
                "ai",
                "ia",
                "llms?",
                "gen ?ai",
                "generative ai",
                "ia generative",
                "intelligence artificielle",
                "machine learning",
                "ai agents?",
                "agentic",
            ],
        ),
        signal("\"Product engineer\" wording", &["product engineers?", "product engineering"]),
        signal("Remote / hybrid", &["remote", "teletravail", "full remote", "hybrid", "hybride"]),
    ]
});

pub fn signals() -> &'static [Signal] {
    &SIGNALS
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::textutil::fold;

    fn skill_named(name: &str) -> &'static Skill {
        taxonomy().iter().find(|s| s.name == name).unwrap_or_else(|| panic!("{name} not in taxonomy"))
    }

    #[test]
    fn taxonomy_matches_real_phrasings() {
        for (skill, text, want) in [
            ("AI / LLM engineering", "Building RAG pipelines with LangChain", true),
            ("AI / LLM engineering", "Expérience en IA générative appréciée", true),
            ("AI / LLM engineering", "We love dragons", false),
            ("Go", "Stack: Python, Go, TypeScript", true),
            ("Go", "Backend in Go and Postgres", true),
            ("Go", "Ready to go the extra mile", false),
            ("Go", "golang microservices", true),
            ("Java / Spring", "JavaScript only", false),
            ("Java / Spring", "Java 21 / Spring Boot", true),
            ("C# / .NET", "C# et .NET 8", true),
            ("C / C++", "Modern C++ (C++20)", true),
            ("Node.js", "Node.js / NestJS", true),
            ("Machine learning / MLOps", "HTML and XML experts", false),
            ("Product engineering", "We hire Product Engineers who talk to users", true),
            ("CI/CD & DevOps", "pipelines CI/CD GitLab", true),
            ("Security / DevSecOps", "Sensibilité à la cybersécurité", true),
            ("Kubernetes", "K8s clusters", true),
            ("Docker / containers", "conteneurisation Docker", true),
        ] {
            assert_eq!(skill_named(skill).matches(&fold(text)), want, "{skill} on {text:?}");
        }
    }

    #[test]
    fn names_are_unique() {
        let names: HashSet<&str> = taxonomy().iter().map(|s| s.name).collect();
        assert_eq!(names.len(), taxonomy().len());
        assert_eq!(signals().len(), 3);
    }
}
