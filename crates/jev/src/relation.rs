//! 将 TypeSafe Choice 问题与答案映射为有界关系判断；不请求自由文本。
use super::{
    network_error, normalize_base_url, protocol_error, reject_secret_in_model, success, Credential,
    InferenceResponse, JevClient,
};
use codexflow_domain::{
    AppError, ErrorCode, EvidenceOption, InferredRelationKind, JevChoiceAnswer, JevEvidenceChoice,
    JevEvidenceSelection, JevRelationClassification, RelationCandidate, RelationChoice,
    RelationJudgment,
};
use reqwest::header;
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, time::Duration};

/// 关系题目、证据选项或解释规则变化时递增，防止复用旧判断。
pub const RELATION_RULES_VERSION: &str = "v1-12-choice-2";

const KINDS: &[(InferredRelationKind, &str)] = &[
    (
        InferredRelationKind::Continues,
        "后续会话延续前序会话未完成的同一工作",
    ),
    (
        InferredRelationKind::Implements,
        "后续会话落实前序会话提出的具体方案",
    ),
    (
        InferredRelationKind::Fixes,
        "后续会话修复前序会话暴露的具体问题",
    ),
    (
        InferredRelationKind::Validates,
        "后续会话验证前序会话的具体结果或结论",
    ),
    (
        InferredRelationKind::Investigates,
        "后续会话调查前序会话提出的问题或现象",
    ),
    (
        InferredRelationKind::Supersedes,
        "后续会话明确取代前序会话的方案或结果",
    ),
    (
        InferredRelationKind::MotivatedBy,
        "后续会话因前序会话的发现或决定而展开",
    ),
    (
        InferredRelationKind::AlternativeTo,
        "两条会话给同一目标提供互斥或可替代方案",
    ),
    (
        InferredRelationKind::Related,
        "两条会话有具体工作联系，但不满足其他类型",
    ),
];

fn answer(answer: &super::ChoiceAnswer, options: &[String]) -> Result<JevChoiceAnswer, AppError> {
    if answer.kind != "choice"
        || !options.contains(&answer.choice)
        || answer.probabilities.len() != options.len()
        || !(0.0..=1.0).contains(&answer.confidence)
        || options.iter().any(|option| {
            !answer
                .probabilities
                .get(option)
                .is_some_and(|value| (0.0..=1.0).contains(value))
        })
        || (answer.probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
        || answer.probabilities[&answer.choice]
            < *answer
                .probabilities
                .values()
                .max_by(|a, b| a.total_cmp(b))
                .unwrap()
    {
        return Err(protocol_error());
    }
    Ok(JevChoiceAnswer {
        choice: answer.choice.clone(),
        confidence: answer.confidence,
        probabilities: answer.probabilities.clone(),
    })
}

fn valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

type ClassificationSpec = (String, InferredRelationKind, String, String);

fn classification_questions(
    candidate: &RelationCandidate,
) -> (Map<String, Value>, Vec<ClassificationSpec>) {
    let mut questions = Map::new();
    let mut specifications = Vec::new();
    for &(kind, meaning) in KINDS {
        for reverse in 0..(if kind.directed() { 2 } else { 1 }) {
            let (from, to, direction) = if reverse == 0 {
                (
                    &candidate.left_thread_id,
                    &candidate.right_thread_id,
                    "leftThread → rightThread",
                )
            } else {
                (
                    &candidate.right_thread_id,
                    &candidate.left_thread_id,
                    "rightThread → leftThread",
                )
            };
            let key = format!(
                "{}_{}",
                kind.as_str().to_lowercase(),
                if reverse == 0 { "ab" } else { "ba" }
            );
            let (instructions, support, reject) = if kind.directed() {
                (format!("独立判断具体关系 {key}：{meaning}。方向为 {direction}，即前序 {from} 指向后续 {to}。只能根据 state 中双方实际材料判断；同一对可有多种关系，不依赖其他题的答案。"),
                    "双方材料明确支持这一具体类型和方向", "双方材料不支持这一具体类型和方向，或明显是另一回事")
            } else {
                (format!("独立判断具体无向关系 {key}：{meaning}。双方是 leftThread {from} 与 rightThread {to}；端点顺序仅用于稳定身份，不表示时间先后或因果。只能根据 state 中双方实际材料判断；同一对可有多种关系，不依赖其他题的答案。"),
                    "双方材料明确支持这一具体无向关系", "双方材料不支持这一具体无向关系，或明显是另一回事")
            };
            questions.insert(key.clone(), json!({
                "type": "choice", "instructions": instructions,
                "criteria": {"SUPPORTS": support, "REJECTS": reject, "UNKNOWN": "材料不足或有歧义，无法判断"}
            }));
            specifications.push((key, kind, from.clone(), to.clone()));
        }
    }
    (questions, specifications)
}

fn selection_questions(
    supported: &[RelationChoice],
    count: usize,
) -> (Map<String, Value>, Vec<String>) {
    let mut options: BTreeMap<String, String> = BTreeMap::new();
    options.insert(
        "INSUFFICIENT".into(),
        "没有任何一组双方证据足以支持此命题".into(),
    );
    for index in 0..count {
        options.insert(
            format!("p{index}"),
            format!("选择 state.evidencePairs 中 option=p{index} 的双侧原始摘录"),
        );
    }
    let mut questions = Map::new();
    for choice in supported {
        let instructions = if choice.kind.directed() {
            format!("独立选择一组最能支持 {} 的双侧证据：前序 {} → 后续 {}；若没有充分双侧证据，选 INSUFFICIENT。不要依赖其他题的答案。", choice.kind.as_str(), choice.from_thread_id, choice.to_thread_id)
        } else {
            format!("独立选择一组最能支持无向关系 {} 的双侧证据：会话 {} 与会话 {}；端点顺序不表示时间先后或因果。若没有充分双侧证据，选 INSUFFICIENT。不要依赖其他题的答案。", choice.kind.as_str(), choice.from_thread_id, choice.to_thread_id)
        };
        questions.insert(
            choice.key.clone(),
            json!({
                "type": "choice", "instructions": instructions, "criteria": options
            }),
        );
    }
    (questions, options.keys().cloned().collect())
}

fn request_characters(state: &Value, model: &str, questions: &Map<String, Value>) -> usize {
    json!({"state": state, "model": model, "questions": questions})
        .to_string()
        .chars()
        .count()
}

fn input_limit_error() -> AppError {
    AppError::jev(
        ErrorCode::JevInvalidRequest,
        "Jev 完整请求超过本次输入字符上限。",
        false,
    )
}

pub struct JevRelationAnalyzer {
    client: JevClient,
    material_limit: usize,
}

impl JevRelationAnalyzer {
    pub fn with_timeout(timeout: Duration) -> Result<Self, AppError> {
        Self::with_timeout_and_material_limit(timeout, 40_000)
    }

    pub fn with_timeout_and_material_limit(
        timeout: Duration,
        material_limit: usize,
    ) -> Result<Self, AppError> {
        Ok(Self {
            client: JevClient::with_timeout(timeout)?,
            material_limit,
        })
    }

    fn states(candidate: &RelationCandidate, count: usize) -> (Value, Value) {
        let sampled = candidate.evidence.pairs.iter().take(count);
        let classification = json!({
            "leftThread": candidate.left_thread_id, "rightThread": candidate.right_thread_id,
            "candidateReasons": candidate.reasons,
            "sampledEvidencePairs": sampled.clone().map(|pair| json!({
                "left": {"excerpt": pair.left.excerpt, "turn": pair.left.turn_id},
                "right": {"excerpt": pair.right.excerpt, "turn": pair.right.turn_id}
            })).collect::<Vec<_>>()
        });
        let selection = json!({
            "leftThread": candidate.left_thread_id, "rightThread": candidate.right_thread_id,
            "evidencePairs": sampled.enumerate().map(|(index, pair)| json!({
                "option": format!("p{index}"),
                "left": {"excerpt": pair.left.excerpt, "turn": pair.left.turn_id},
                "right": {"excerpt": pair.right.excerpt, "turn": pair.right.turn_id}
            })).collect::<Vec<_>>()
        });
        (classification, selection)
    }

    fn prepare_classification(
        &self,
        candidate: &RelationCandidate,
        model: &str,
    ) -> Result<(Value, Map<String, Value>, Vec<ClassificationSpec>), AppError> {
        if candidate.left_thread_id == candidate.right_thread_id
            || candidate.evidence.pairs.len() > 20
        {
            return Err(protocol_error());
        }
        let (questions, specifications) = classification_questions(candidate);
        for count in (0..=candidate.evidence.pairs.len()).rev() {
            let (state, _) = Self::states(candidate, count);
            if request_characters(&state, model, &questions) <= self.material_limit {
                return Ok((state, questions, specifications));
            }
        }
        Err(input_limit_error())
    }

    fn prepare_selection(
        &self,
        candidate: &RelationCandidate,
        supported: &[RelationChoice],
        model: &str,
    ) -> Result<(Value, Map<String, Value>, Vec<String>, Vec<EvidenceOption>), AppError> {
        if supported.is_empty() || candidate.evidence.pairs.len() > 20 {
            return Err(protocol_error());
        }
        for count in (0..=candidate.evidence.pairs.len()).rev() {
            let (_, state) = Self::states(candidate, count);
            let (questions, options) = selection_questions(supported, count);
            if request_characters(&state, model, &questions) <= self.material_limit {
                let evidence_options = candidate
                    .evidence
                    .pairs
                    .iter()
                    .take(count)
                    .enumerate()
                    .map(|(index, pair)| EvidenceOption {
                        key: format!("p{index}"),
                        pair: pair.clone(),
                    })
                    .collect();
                return Ok((state, questions, options, evidence_options));
            }
        }
        Err(input_limit_error())
    }

    pub fn validate_classification(
        &self,
        candidate: &RelationCandidate,
        model: &str,
    ) -> Result<(), AppError> {
        self.prepare_classification(candidate, model).map(|_| ())
    }

    pub fn validate_evidence_selection(
        &self,
        candidate: &RelationCandidate,
        supported: &[RelationChoice],
        model: &str,
    ) -> Result<(), AppError> {
        self.prepare_selection(candidate, supported, model)
            .map(|_| ())
    }

    async fn submit(
        &self,
        credential: &Credential,
        model: &str,
        state: Value,
        questions: Map<String, Value>,
    ) -> Result<InferenceResponse, AppError> {
        reject_secret_in_model(credential, model)?;
        let payload = json!({"state": state, "model": model, "questions": questions});
        if payload.to_string().chars().count() > self.material_limit {
            return Err(input_limit_error());
        }
        let url = format!("{}/v1/systemone", normalize_base_url(&credential.base_url)?);
        let response = self
            .client
            .client
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .bearer_auth(&credential.key)
            .json(&payload)
            .send()
            .await
            .map_err(network_error)?;
        let body: InferenceResponse = success(response)
            .await?
            .json()
            .await
            .map_err(|_| protocol_error())?;
        if !valid_model_id(&body.model)
            || body.model.contains(&credential.key)
            || body.answers.len() != questions.len()
        {
            return Err(protocol_error());
        }
        Ok(body)
    }

    pub async fn classify(
        &self,
        credential: &Credential,
        model: &str,
        candidate: &RelationCandidate,
    ) -> Result<JevRelationClassification, AppError> {
        let (state, questions, specifications) = self.prepare_classification(candidate, model)?;
        let body = self.submit(credential, model, state, questions).await?;
        let options = ["SUPPORTS".into(), "REJECTS".into(), "UNKNOWN".into()];
        let mut choices = Vec::new();
        for (key, kind, from, to) in specifications {
            let parsed = answer(body.answers.get(&key).ok_or_else(protocol_error)?, &options)?;
            let judgment = match parsed.choice.as_str() {
                "SUPPORTS" => RelationJudgment::Supported,
                "REJECTS" => RelationJudgment::Rejected,
                _ => RelationJudgment::Unknown,
            };
            choices.push(RelationChoice {
                key,
                kind,
                from_thread_id: from,
                to_thread_id: to,
                judgment,
                answer: parsed,
            });
        }
        Ok(JevRelationClassification {
            requested_model: model.into(),
            actual_model: body.model,
            choices,
            input_tokens: body.usage.input_tokens,
            output_tokens: body.usage.output_tokens,
        })
    }

    pub async fn select_evidence(
        &self,
        credential: &Credential,
        model: &str,
        candidate: &RelationCandidate,
        supported: &[RelationChoice],
    ) -> Result<JevEvidenceSelection, AppError> {
        let (state, questions, keys, evidence_options) =
            self.prepare_selection(candidate, supported, model)?;
        let body = self.submit(credential, model, state, questions).await?;
        let mut choices = Vec::new();
        for relation in supported {
            let parsed = answer(
                body.answers.get(&relation.key).ok_or_else(protocol_error)?,
                &keys,
            )?;
            let pair_id = if parsed.choice == "INSUFFICIENT" {
                None
            } else {
                let index: usize = parsed
                    .choice
                    .strip_prefix('p')
                    .ok_or_else(protocol_error)?
                    .parse()
                    .map_err(|_| protocol_error())?;
                Some(
                    candidate
                        .evidence
                        .pairs
                        .get(index)
                        .ok_or_else(protocol_error)?
                        .id
                        .clone(),
                )
            };
            choices.push(JevEvidenceChoice {
                relation_key: relation.key.clone(),
                pair_id,
                answer: parsed,
            });
        }
        Ok(JevEvidenceSelection {
            actual_model: body.model,
            choices,
            evidence_options,
            input_tokens: body.usage.input_tokens,
            output_tokens: body.usage.output_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexflow_domain::{
        CandidateEvidence, CandidateReason, EvidenceField, EvidencePair, EvidenceSample,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn candidate() -> RelationCandidate {
        let evidence = |id: &str, thread: &str, excerpt: &str| CandidateEvidence {
            id: id.into(),
            thread_id: thread.into(),
            turn_id: "turn".into(),
            item_id: "item".into(),
            field: EvidenceField::Text,
            change_index: None,
            excerpt: excerpt.into(),
            content_version: "v1".into(),
            fact_id: None,
        };
        RelationCandidate {
            id: "candidate-1".into(),
            left_thread_id: "thread-a".into(),
            right_thread_id: "thread-b".into(),
            score: 12,
            reasons: vec![CandidateReason {
                signal: "text".into(),
                detail: "共同目标".into(),
            }],
            evidence: EvidenceSample {
                left_available: 1,
                right_available: 1,
                combinations_available: 1,
                combinations_shown: 1,
                left_sampled: 1,
                right_sampled: 1,
                sampling_rule: "test".into(),
                pairs: vec![EvidencePair {
                    id: "pair-1".into(),
                    left: evidence("a", "thread-a", "发现中文故障"),
                    right: evidence("b", "thread-b", "修复中文故障"),
                }],
            },
        }
    }

    fn long_candidate() -> RelationCandidate {
        let mut candidate = candidate();
        candidate.evidence.pairs = (0..20)
            .map(|index| {
                let mut pair = candidate.evidence.pairs[0].clone();
                pair.id = format!("pair-{index}");
                pair.left.excerpt = "中文".repeat(120);
                pair.right.excerpt = "技术".repeat(120);
                pair
            })
            .collect();
        candidate
    }

    fn server(
        make_response: impl Fn(Value) -> Value + Send + 'static,
    ) -> (String, thread::JoinHandle<Value>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 8192];
            let request = loop {
                let count = stream.read(&mut buffer).unwrap();
                bytes.extend_from_slice(&buffer[..count]);
                let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..split]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse().ok())
                    })
                    .unwrap();
                if bytes.len() >= split + 4 + length {
                    assert!(headers.starts_with("POST /v1/systemone HTTP/1.1"));
                    assert!(headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer synthetic-key"));
                    break serde_json::from_slice::<Value>(&bytes[split + 4..split + 4 + length])
                        .unwrap();
                }
            };
            let body = make_response(request.clone()).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            request
        });
        (url, handle)
    }

    fn credential(url: String) -> Credential {
        Credential {
            base_url: url,
            key: "synthetic-key".into(),
        }
    }
    fn choice(value: &str, options: &[&str]) -> Value {
        let mut probabilities = Map::new();
        for option in options {
            probabilities.insert(
                (*option).into(),
                json!(if *option == value {
                    0.8
                } else {
                    0.2 / (options.len() - 1) as f64
                }),
            );
        }
        json!({"type":"choice", "choice":value, "confidence":0.74, "probabilities":probabilities})
    }
    fn response(request: &Value, chosen: impl Fn(&str, &[&str]) -> Value) -> Value {
        let answers: Map<String, Value> = request["questions"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                let options: Vec<&str> = value["criteria"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                (key.clone(), chosen(key, &options))
            })
            .collect();
        json!({"model":"jev-1.13.0","answers":answers,"usage":{"input_tokens":24,"output_tokens":8}})
    }

    #[tokio::test]
    async fn classification_uses_independent_directional_choice_questions_and_chinese_material() {
        let (url, handle) = server(|request| {
            response(&request, |key, options| {
                let answer = if ["fixes_ab", "validates_ba", "related_ab"].contains(&key) {
                    "SUPPORTS"
                } else if key == "investigates_ab" {
                    "UNKNOWN"
                } else {
                    "REJECTS"
                };
                choice(answer, options)
            })
        });
        let classification = JevRelationAnalyzer::with_timeout(Duration::from_secs(3))
            .unwrap()
            .classify(&credential(url), "jev-latest", &candidate())
            .await
            .unwrap();
        assert_eq!(classification.choices.len(), 16);
        for &(kind, _) in KINDS {
            let ab = format!("{}_ab", kind.as_str().to_lowercase());
            assert!(classification.choices.iter().any(|item| item.key == ab
                && item.from_thread_id == "thread-a"
                && item.to_thread_id == "thread-b"));
            let ba = format!("{}_ba", kind.as_str().to_lowercase());
            assert_eq!(
                classification.choices.iter().any(|item| item.key == ba
                    && item.from_thread_id == "thread-b"
                    && item.to_thread_id == "thread-a"),
                kind.directed()
            );
        }
        assert_eq!(
            classification
                .choices
                .iter()
                .filter(|item| item.judgment == RelationJudgment::Supported)
                .count(),
            3
        );
        assert!(classification
            .choices
            .iter()
            .any(|item| item.key == "validates_ba" && item.from_thread_id == "thread-b"));
        assert!(classification.choices.iter().any(
            |item| item.key == "investigates_ab" && item.judgment == RelationJudgment::Unknown
        ));
        assert_eq!(classification.actual_model, "jev-1.13.0");
        let request = handle.join().unwrap();
        assert_eq!(request["model"], "jev-latest");
        assert!(
            request["state"]["sampledEvidencePairs"][0]["left"]["excerpt"]
                .as_str()
                .unwrap()
                .contains("中文")
        );
        assert_eq!(request["questions"]["fixes_ab"]["type"], "choice");
        assert_eq!(
            request["questions"]["fixes_ab"]["criteria"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn selection_uses_one_request_for_multiple_supported_claims_and_can_choose_insufficient()
    {
        let supported = vec![
            RelationChoice {
                key: "fixes_ab".into(),
                kind: InferredRelationKind::Fixes,
                from_thread_id: "thread-a".into(),
                to_thread_id: "thread-b".into(),
                judgment: RelationJudgment::Supported,
                answer: JevChoiceAnswer {
                    choice: "SUPPORTS".into(),
                    confidence: 0.8,
                    probabilities: BTreeMap::from([
                        ("SUPPORTS".into(), 0.8),
                        ("REJECTS".into(), 0.1),
                        ("UNKNOWN".into(), 0.1),
                    ]),
                },
            },
            RelationChoice {
                key: "related_ab".into(),
                kind: InferredRelationKind::Related,
                from_thread_id: "thread-a".into(),
                to_thread_id: "thread-b".into(),
                judgment: RelationJudgment::Supported,
                answer: JevChoiceAnswer {
                    choice: "SUPPORTS".into(),
                    confidence: 0.8,
                    probabilities: BTreeMap::from([
                        ("SUPPORTS".into(), 0.8),
                        ("REJECTS".into(), 0.1),
                        ("UNKNOWN".into(), 0.1),
                    ]),
                },
            },
        ];
        let (url, handle) = server(|request| {
            response(&request, |key, options| {
                choice(
                    if key == "fixes_ab" {
                        "p0"
                    } else {
                        "INSUFFICIENT"
                    },
                    options,
                )
            })
        });
        let result = JevRelationAnalyzer::with_timeout(Duration::from_secs(3))
            .unwrap()
            .select_evidence(&credential(url), "jev-latest", &candidate(), &supported)
            .await
            .unwrap();
        assert_eq!(result.choices.len(), 2);
        assert_eq!(result.choices[0].pair_id.as_deref(), Some("pair-1"));
        assert!(result.choices[1].pair_id.is_none());
        let request = handle.join().unwrap();
        assert_eq!(request["questions"].as_object().unwrap().len(), 2);
        assert_eq!(
            request["questions"]["fixes_ab"]["criteria"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn rejects_missing_keys_unknown_options_and_invalid_scores() {
        for mutation in ["missing", "option", "confidence", "probability", "type"] {
            let (url, handle) = server(move |request| {
                let mut body = response(&request, |_, options| choice("REJECTS", options));
                match mutation {
                    "missing" => {
                        body["answers"].as_object_mut().unwrap().remove("fixes_ab");
                    }
                    "option" => body["answers"]["fixes_ab"]["choice"] = json!("outside-id"),
                    "confidence" => body["answers"]["fixes_ab"]["confidence"] = json!(1.2),
                    "probability" => {
                        body["answers"]["fixes_ab"]["probabilities"]["REJECTS"] = json!(1.2)
                    }
                    _ => body["answers"]["fixes_ab"]["type"] = json!("noul"),
                }
                body
            });
            let error = JevRelationAnalyzer::with_timeout(Duration::from_secs(3))
                .unwrap()
                .classify(&credential(url), "jev-latest", &candidate())
                .await
                .unwrap_err();
            assert!(
                matches!(error.code, codexflow_domain::ErrorCode::JevProtocolInvalid),
                "{mutation}"
            );
            handle.join().unwrap();
        }
    }

    #[tokio::test]
    async fn full_request_limit_reduces_the_number_of_sent_evidence_pairs() {
        let candidate = long_candidate();
        let (questions, _) = classification_questions(&candidate);
        let base = request_characters(
            &JevRelationAnalyzer::states(&candidate, 0).0,
            "jev-latest",
            &questions,
        );
        let full = request_characters(
            &JevRelationAnalyzer::states(&candidate, 20).0,
            "jev-latest",
            &questions,
        );
        let limit = base + (full - base) / 2;
        let (url, handle) =
            server(|request| response(&request, |_, options| choice("REJECTS", options)));
        JevRelationAnalyzer::with_timeout_and_material_limit(Duration::from_secs(3), limit)
            .unwrap()
            .classify(&credential(url), "jev-latest", &candidate)
            .await
            .unwrap();
        let request = handle.join().unwrap();
        let sent = request["state"]["sampledEvidencePairs"]
            .as_array()
            .unwrap()
            .len();
        assert!(sent > 0 && sent < 20);
        assert!(request.to_string().chars().count() <= limit);
    }

    #[tokio::test]
    async fn multiple_evidence_questions_count_repeated_choices_in_the_full_request() {
        let candidate = long_candidate();
        let supported: Vec<_> = (0..8)
            .map(|index| RelationChoice {
                key: format!("fixes_ab_{index}"),
                kind: InferredRelationKind::Fixes,
                from_thread_id: "thread-a".into(),
                to_thread_id: "thread-b".into(),
                judgment: RelationJudgment::Supported,
                answer: JevChoiceAnswer {
                    choice: "SUPPORTS".into(),
                    confidence: 0.8,
                    probabilities: BTreeMap::from([
                        ("SUPPORTS".into(), 0.8),
                        ("REJECTS".into(), 0.1),
                        ("UNKNOWN".into(), 0.1),
                    ]),
                },
            })
            .collect();
        let size = |count| {
            let (_, state) = JevRelationAnalyzer::states(&candidate, count);
            let (questions, _) = selection_questions(&supported, count);
            request_characters(&state, "jev-latest", &questions)
        };
        let base = size(0);
        let full = size(20);
        assert!(full > base);
        let tiny =
            JevRelationAnalyzer::with_timeout_and_material_limit(Duration::from_secs(3), base - 1)
                .unwrap();
        assert!(matches!(
            tiny.validate_evidence_selection(&candidate, &supported, "jev-latest")
                .unwrap_err()
                .code,
            ErrorCode::JevInvalidRequest
        ));
        let limit = base + (full - base) / 3;
        let (url, handle) =
            server(|request| response(&request, |_, options| choice("INSUFFICIENT", options)));
        let selected =
            JevRelationAnalyzer::with_timeout_and_material_limit(Duration::from_secs(3), limit)
                .unwrap()
                .select_evidence(&credential(url), "jev-latest", &candidate, &supported)
                .await
                .unwrap();
        assert!(selected.choices.iter().all(|item| item.pair_id.is_none()));
        let request = handle.join().unwrap();
        let sent = request["state"]["evidencePairs"].as_array().unwrap().len();
        assert!(sent > 0 && sent < 20);
        assert_eq!(request["questions"].as_object().unwrap().len(), 8);
        assert_eq!(
            request["questions"]["fixes_ab_0"]["criteria"]
                .as_object()
                .unwrap()
                .len(),
            sent + 1
        );
        assert!(request.to_string().chars().count() <= limit);
    }

    #[tokio::test]
    async fn undirected_questions_do_not_describe_a_causal_order() {
        let (url, handle) = server(|request| {
            response(&request, |key, options| {
                choice(
                    if key == "related_ab" || key == "alternative_to_ab" {
                        "SUPPORTS"
                    } else {
                        "REJECTS"
                    },
                    options,
                )
            })
        });
        let classification = JevRelationAnalyzer::with_timeout(Duration::from_secs(3))
            .unwrap()
            .classify(&credential(url), "jev-latest", &candidate())
            .await
            .unwrap();
        let request = handle.join().unwrap();
        for key in ["related_ab", "alternative_to_ab"] {
            let question = &request["questions"][key];
            let instructions = question["instructions"].as_str().unwrap();
            assert!(instructions.contains("无向关系"));
            assert!(
                !instructions.contains("前序")
                    && !instructions.contains("后续")
                    && !instructions.contains('→')
            );
            assert!(!question["criteria"].to_string().contains("方向"));
        }
        assert!(request["questions"]["fixes_ab"]["instructions"]
            .as_str()
            .unwrap()
            .contains("前序"));
        let supported: Vec<_> = classification
            .choices
            .into_iter()
            .filter(|item| item.judgment == RelationJudgment::Supported)
            .collect();
        let (url, handle) =
            server(|request| response(&request, |_, options| choice("p0", options)));
        JevRelationAnalyzer::with_timeout(Duration::from_secs(3))
            .unwrap()
            .select_evidence(&credential(url), "jev-latest", &candidate(), &supported)
            .await
            .unwrap();
        let request = handle.join().unwrap();
        for key in ["related_ab", "alternative_to_ab"] {
            let instructions = request["questions"][key]["instructions"].as_str().unwrap();
            assert!(instructions.contains("无向关系"));
            assert!(
                !instructions.contains("前序")
                    && !instructions.contains("后续")
                    && !instructions.contains('→')
            );
        }
    }
}
