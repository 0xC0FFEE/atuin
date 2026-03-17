use minspan::minspan;

use super::{history::History, settings::SearchMode};

pub fn reorder_fuzzy(mode: SearchMode, query: &str, res: Vec<History>) -> Vec<History> {
    match mode {
        SearchMode::Fuzzy | SearchMode::Nucleo => reorder(query, |x| &x.command, res),
        _ => res,
    }
}

fn reorder<F, A>(query: &str, f: F, res: Vec<A>) -> Vec<A>
where
    F: Fn(&A) -> &String,
    A: Clone,
{
    let mut r = res.clone();
    let qvec = &query.chars().collect();
    r.sort_by_cached_key(|h| {
        // TODO for fzf search we should sum up scores for each matched term
        let (from, to) = match minspan::span(qvec, &(f(h).chars().collect())) {
            Some(x) => x,
            // this is a little unfortunate: when we are asked to match a query that is found nowhere,
            // we don't want to return a None, as the comparison behaviour would put the worst matches
            // at the front. therefore, we'll return a set of indices that are one larger than the longest
            // possible legitimate match. This is meaningless except as a comparison.
            None => (0, res.len()),
        };
        1 + to - from
    });
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use time::OffsetDateTime;

    #[test]
    fn nucleo_reorders_like_fuzzy() {
        let q = "abc";
        let mk = |command: &str| History {
            id: "id".to_string().into(),
            timestamp: OffsetDateTime::UNIX_EPOCH,
            duration: 0,
            exit: 0,
            command: command.to_string(),
            cwd: "/".to_string(),
            session: "session".to_string(),
            hostname: "host:user".to_string(),
            author: "user".to_string(),
            intent: None,
            deleted_at: None,
        };

        let input = vec![mk("a___b___c"), mk("abc"), mk("zzz")];

        let fuzzy = reorder_fuzzy(SearchMode::Fuzzy, q, input.clone());
        let nucleo = reorder_fuzzy(SearchMode::Nucleo, q, input);

        assert_eq!(
            fuzzy.iter().map(|h| &h.command).collect::<Vec<_>>(),
            nucleo.iter().map(|h| &h.command).collect::<Vec<_>>()
        );
    }
}
