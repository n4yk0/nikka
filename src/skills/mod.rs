//! Measures which skills job posts ask for, using a regex taxonomy over accent-folded text.

mod hn;
mod taxonomy;

pub use hn::{HN_ITEM_URL, HN_THREADS_URL, HiringCorpora, Thread, fetch_hn, hiring_threads, thread_posts};
pub use taxonomy::{Signal, Skill, signals, taxonomy};

use crate::textutil::fold;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Corpus {
    pub name: String,
    pub label: String,
    pub docs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CorpusInfo {
    pub name: String,
    pub label: String,
    pub docs: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stat {
    pub name: &'static str,
    pub category: &'static str,
    /// Share of documents matching, per corpus, in `Analysis::corpora` order.
    pub shares: Vec<f64>,
    pub combined: f64,
    /// Share change in percentage points against the baseline.
    pub delta: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub corpora: Vec<CorpusInfo>,
    pub skills: Vec<Stat>,
    pub signals: Vec<Stat>,
}

impl Analysis {
    pub fn top(&self, n: usize) -> &[Stat] {
        &self.skills[..n.min(self.skills.len())]
    }
}

/// Weights every corpus equally so a 500-post HN thread does not drown a 150-offer French sample.
/// With a baseline, `delta` compares it to the corpus sharing its name.
pub fn analyze(corpora: &[Corpus], baseline: Option<&Corpus>) -> Analysis {
    let kept: Vec<&Corpus> = corpora.iter().filter(|c| !c.docs.is_empty()).collect();
    let folded: Vec<Vec<String>> = kept.iter().map(|c| fold_all(&c.docs)).collect();
    let baseline = baseline.filter(|b| !b.docs.is_empty()).and_then(|b| {
        let idx = kept.iter().position(|c| c.name == b.name)?;
        Some((idx, fold_all(&b.docs)))
    });

    let measure = |name: &'static str, category: &'static str, matches: &dyn Fn(&str) -> bool| {
        let shares: Vec<f64> = folded.iter().map(|docs| share(docs, matches)).collect();
        let combined = if shares.is_empty() { 0.0 } else { shares.iter().sum::<f64>() / shares.len() as f64 };
        let delta = baseline.as_ref().map(|(idx, docs)| (shares[*idx] - share(docs, matches)) * 100.0);
        Stat { name, category, shares, combined, delta }
    };

    let mut skills: Vec<Stat> = taxonomy().iter().map(|s| measure(s.name, s.category, &|d| s.matches(d))).collect();
    skills.sort_by(|a, b| b.combined.total_cmp(&a.combined));
    let signals = signals().iter().map(|s| measure(s.name, "", &|d| s.matches(d))).collect();
    Analysis {
        corpora: kept
            .iter()
            .map(|c| CorpusInfo { name: c.name.clone(), label: c.label.clone(), docs: c.docs.len() })
            .collect(),
        skills,
        signals,
    }
}

fn fold_all(docs: &[String]) -> Vec<String> {
    docs.iter().map(|d| fold(d)).collect()
}

fn share(docs: &[String], matches: &dyn Fn(&str) -> bool) -> f64 {
    if docs.is_empty() {
        return 0.0;
    }
    docs.iter().filter(|d| matches(d)).count() as f64 / docs.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(name: &str, docs: &[&str]) -> Corpus {
        Corpus { name: name.into(), label: name.into(), docs: docs.iter().map(|d| d.to_string()).collect() }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn shares_combined_and_delta() {
        let hn = corpus(
            "hn",
            &[
                "Senior engineer, Python and LLM agents, remote",
                "Product Engineer, TypeScript + React",
                "Python backend, Kubernetes",
                "Rust systems",
            ],
        );
        let fr = corpus("fr", &["Développeur Python, télétravail partiel", "Développeur PHP Symfony"]);
        let previous =
            corpus("hn", &["Python", "Java", "Go, Rust", "PHP", "Ruby", "Elixir", "Scala", "Kotlin", "Swift", "C#"]);
        let a = analyze(&[hn, fr, corpus("empty", &[])], Some(&previous));
        assert_eq!(a.corpora.len(), 2, "empty corpora are skipped");
        let top = &a.top(1)[0];
        assert_eq!(top.name, "Python");
        assert!(close(top.shares[0], 0.5) && close(top.shares[1], 0.5) && close(top.combined, 0.5), "{top:?}");
        assert!(top.delta.is_some_and(|d| close(d, 40.0)), "{top:?}");

        let signal = |name: &str| a.signals.iter().find(|s| s.name == name).unwrap();
        let ai = signal("AI in the job description");
        assert!(close(ai.shares[0], 0.25) && ai.shares[1] == 0.0, "{ai:?}");
        assert!(close(signal("Remote / hybrid").shares[1], 0.5), "French télétravail counts as remote");
        assert!(close(signal("\"Product engineer\" wording").shares[0], 0.25));
    }

    #[test]
    fn no_corpora_or_unmatched_baseline() {
        let a = analyze(&[], None);
        assert!(a.corpora.is_empty() && a.skills[0].combined == 0.0 && a.skills[0].delta.is_none());
        assert_eq!(a.top(1000).len(), taxonomy().len());
        let unmatched = analyze(&[corpus("fr", &["x"])], Some(&corpus("hn", &["python"])));
        assert!(unmatched.skills.iter().all(|s| s.delta.is_none()));
    }
}
