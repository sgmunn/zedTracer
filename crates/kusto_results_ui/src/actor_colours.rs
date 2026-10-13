//! The colours of a trace's actors. The Timeline and the Sequence diagram take them from here, so
//! that one actor is one colour in both.
//!
//! An actor's colour depends only on its place among the actors that have a name, in the order
//! they are first seen, which the Timeline numbers by and the Sequence model records as
//! `Actor::named_index`.

use std::sync::Arc;

use gpui::{App, Hsla, hsla};
use markdown::MermaidAccentColors;
use ui::prelude::*;

/// The hues, in degrees, that actors take in turn, no two closer than 24 degrees and in an order
/// that keeps the first ones far apart.
const ACTOR_HUES: [f32; 10] = [210., 28., 285., 135., 350., 175., 52., 320., 240., 85.];

/// One colour for each of `count` actors, all different, at one saturation and lightness. Past
/// the last hue they come round again, lighter and then darker, so that a trace with more actors
/// than hues still tells neighbours apart. An actor's colour does not depend on `count`.
fn actor_palette(count: usize, saturation: f32, lightness: f32) -> Vec<Hsla> {
    (0..count)
        .map(|actor| {
            let shift = [0.0, 0.12, -0.12][(actor / ACTOR_HUES.len()) % 3];
            hsla(
                ACTOR_HUES[actor % ACTOR_HUES.len()] / 360.,
                saturation,
                (lightness + shift).clamp(0.25, 0.8),
                1.,
            )
        })
        .collect()
}

/// The actor colours for the current theme. The saturation and lightness are the average of the
/// theme's player colours, so they sit well on it, but the hues are not the players': a theme can
/// hold two of nearly the same colour (One Light has two purples), and the actors must not.
pub(crate) fn theme_actor_palette(count: usize, cx: &App) -> Vec<Hsla> {
    let mut saturation = 0.;
    let mut lightness = 0.;
    let mut players = 0.;
    // The first player is the local one, which a trace has no part for.
    for player in cx.theme().players().0.iter().skip(1) {
        saturation += player.cursor.s;
        lightness += player.cursor.l;
        players += 1.;
    }
    let (saturation, lightness) = if players > 0. {
        (saturation / players, lightness / players)
    } else {
        (0.5, 0.5)
    };
    actor_palette(count, saturation.clamp(0.35, 0.75), lightness.clamp(0.35, 0.7))
}

/// The colours a sequence diagram gives its participants, which it takes in the order they are
/// declared. Each entry is the place of that participant's actor among the named ones, or `None`
/// for a participant that is no actor of the trace, which looks as a participant does by default.
pub(crate) fn sequence_accents(places: Vec<Option<usize>>) -> MermaidAccentColors {
    Arc::new(move |cx: &App| {
        let count = places.iter().flatten().max().map_or(0, |last| last + 1);
        let palette = theme_actor_palette(count, cx);
        let colors = cx.theme().colors();
        places
            .iter()
            .map(|place| match place.and_then(|place| palette.get(place)) {
                Some(colour) => (*colour, *colour),
                None => (colors.border, colors.element_background),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn theme(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
        });
    }

    #[test]
    fn no_two_actor_hues_are_closer_than_24_degrees() {
        let mut hues = ACTOR_HUES.to_vec();
        hues.sort_by(f32::total_cmp);
        let around = hues
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .chain([hues[0] + 360. - hues[hues.len() - 1]]);
        let closest = around.fold(f32::MAX, f32::min);
        assert!(closest >= 24., "the closest hues are {closest} degrees apart");
    }

    #[test]
    fn every_actor_gets_a_colour_of_its_own_until_the_hues_run_out_and_then_a_lighter_or_darker_one() {
        for (saturation, lightness) in [(0.46, 0.46), (0.47, 0.64)] {
            let palette = actor_palette(ACTOR_HUES.len() * 3, saturation, lightness);
            assert_eq!(palette.len(), 30);
            for (first, a) in palette.iter().enumerate() {
                for b in &palette[first + 1..] {
                    assert!((a.h, a.s, a.l) != (b.h, b.s, b.l), "{a:?} appears twice");
                }
            }
            let (first_round, second_round) = (palette[0], palette[ACTOR_HUES.len()]);
            assert_eq!(first_round.h, second_round.h, "the same hue comes round again");
            assert!(second_round.l > first_round.l, "lighter the second time");
            assert!(palette[ACTOR_HUES.len() * 2].l < first_round.l, "darker the third time");
        }
        assert!(actor_palette(0, 0.5, 0.5).is_empty());
    }

    #[test]
    fn an_actors_colour_does_not_depend_on_how_many_actors_there_are() {
        assert_eq!(actor_palette(3, 0.5, 0.5)[2], actor_palette(25, 0.5, 0.5)[2]);
    }

    #[gpui::test]
    fn the_theme_gives_every_actor_a_different_colour_in_a_usable_range(cx: &mut TestAppContext) {
        theme(cx);
        cx.update(|cx| {
            let palette = theme_actor_palette(7, cx);
            assert_eq!(palette.len(), 7);
            for (first, a) in palette.iter().enumerate() {
                assert!(a.s >= 0.35 && a.s <= 0.75, "{a:?}");
                for b in &palette[first + 1..] {
                    assert_ne!(a.h, b.h);
                }
            }
        });
    }

    #[gpui::test]
    fn a_sequence_diagram_gives_each_participant_the_colour_its_actor_has_in_the_timeline(
        cx: &mut TestAppContext,
    ) {
        theme(cx);
        cx.update(|cx| {
            let timeline = theme_actor_palette(3, cx);
            // The caller outside the result, then the actors in the order the diagram draws them.
            let accents = sequence_accents(vec![None, Some(2), Some(0), None, Some(1)])(cx);
            let colors = cx.theme().colors();
            assert_eq!(
                accents,
                vec![
                    (colors.border, colors.element_background),
                    (timeline[2], timeline[2]),
                    (timeline[0], timeline[0]),
                    (colors.border, colors.element_background),
                    (timeline[1], timeline[1]),
                ]
            );
            assert!(sequence_accents(Vec::new())(cx).is_empty());
        });
    }
}
