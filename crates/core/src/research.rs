//! Citation-first research records with explicit contradiction reporting.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub id: Uuid,
    pub title: String,
    pub url: Url,
    pub retrieved_at: DateTime<Utc>,
    pub content_sha256: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: Uuid,
    pub subject: String,
    pub predicate: String,
    pub value: String,
    pub citation_ids: BTreeSet<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Contradiction {
    pub subject: String,
    pub predicate: String,
    pub claim_ids: Vec<Uuid>,
    pub values: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResearchReport {
    pub id: Uuid,
    pub title: String,
    pub citations: BTreeMap<Uuid, Citation>,
    pub claims: Vec<Claim>,
    pub contradictions: Vec<Contradiction>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResearchProject {
    pub id: Uuid,
    pub title: String,
    pub reports: Vec<ResearchReport>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResearchError {
    #[error("research title, claim subject, predicate, and value cannot be blank")]
    Blank,
    #[error("claim {claim_id} has no citations")]
    UncitedClaim { claim_id: Uuid },
    #[error("claim {claim_id} references missing citation {citation_id}")]
    MissingCitation { claim_id: Uuid, citation_id: Uuid },
}

impl ResearchReport {
    /// Build a report only when every claim has valid provenance.
    ///
    /// # Errors
    /// Rejects blank fields, uncited claims, and broken citation references.
    pub fn build(
        title: &str,
        citations: Vec<Citation>,
        claims: Vec<Claim>,
    ) -> Result<Self, ResearchError> {
        if title.trim().is_empty()
            || claims.iter().any(|claim| {
                claim.subject.trim().is_empty()
                    || claim.predicate.trim().is_empty()
                    || claim.value.trim().is_empty()
            })
        {
            return Err(ResearchError::Blank);
        }
        let citations = citations
            .into_iter()
            .map(|citation| (citation.id, citation))
            .collect::<BTreeMap<_, _>>();
        for claim in &claims {
            if claim.citation_ids.is_empty() {
                return Err(ResearchError::UncitedClaim { claim_id: claim.id });
            }
            if let Some(citation_id) = claim
                .citation_ids
                .iter()
                .find(|id| !citations.contains_key(*id))
            {
                return Err(ResearchError::MissingCitation {
                    claim_id: claim.id,
                    citation_id: *citation_id,
                });
            }
        }
        let contradictions = detect_contradictions(&claims);
        Ok(Self {
            id: Uuid::now_v7(),
            title: title.trim().into(),
            citations,
            claims,
            contradictions,
        })
    }
}

fn detect_contradictions(claims: &[Claim]) -> Vec<Contradiction> {
    let mut groups: BTreeMap<(String, String), Vec<&Claim>> = BTreeMap::new();
    for claim in claims {
        groups
            .entry((normalize(&claim.subject), normalize(&claim.predicate)))
            .or_default()
            .push(claim);
    }
    groups
        .into_iter()
        .filter_map(|((subject, predicate), group)| {
            let values = group
                .iter()
                .map(|claim| normalize(&claim.value))
                .collect::<BTreeSet<_>>();
            (values.len() > 1).then(|| Contradiction {
                subject,
                predicate,
                claim_ids: group.iter().map(|claim| claim.id).collect(),
                values,
            })
        })
        .collect()
}

fn normalize(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claims_require_sources_and_contradictions_remain_visible() {
        let one = Citation {
            id: Uuid::now_v7(),
            title: "one".into(),
            url: Url::parse("https://one.example.test").unwrap(),
            retrieved_at: Utc::now(),
            content_sha256: None,
        };
        let two = Citation {
            id: Uuid::now_v7(),
            title: "two".into(),
            url: Url::parse("https://two.example.test").unwrap(),
            retrieved_at: Utc::now(),
            content_sha256: None,
        };
        let claims = vec![
            Claim {
                id: Uuid::now_v7(),
                subject: "Product".into(),
                predicate: "price".into(),
                value: "$10".into(),
                citation_ids: [one.id].into(),
            },
            Claim {
                id: Uuid::now_v7(),
                subject: " product ".into(),
                predicate: "Price".into(),
                value: "$12".into(),
                citation_ids: [two.id].into(),
            },
        ];
        let report =
            ResearchReport::build("comparison", vec![one, two], claims).expect("cited report");
        assert_eq!(report.contradictions.len(), 1);
        assert_eq!(report.contradictions[0].values.len(), 2);
    }
    #[test]
    fn uncited_claim_is_rejected() {
        let claim = Claim {
            id: Uuid::now_v7(),
            subject: "x".into(),
            predicate: "is".into(),
            value: "y".into(),
            citation_ids: BTreeSet::new(),
        };
        assert!(matches!(
            ResearchReport::build("report", vec![], vec![claim]),
            Err(ResearchError::UncitedClaim { .. })
        ));
    }
}
