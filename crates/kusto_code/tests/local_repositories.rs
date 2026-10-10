use std::collections::HashMap;
use std::fs;
use std::path::Path;

use futures::executor::block_on;
use kusto_code::{
    CodeKeys, CodeRef, CodeRefKind, CodeResolver, EventText, GitSearch, KeyOrigin,
    discover_repositories,
};
use pretty_assertions::assert_eq;

fn write(root: &Path, path: &str, contents: &str) {
    let full_path = root.join(path);
    fs::create_dir_all(full_path.parent().expect("a parent folder")).expect("a folder");
    fs::write(full_path, contents).expect("a file");
}

fn track(repository: &Path) {
    for arguments in [["init", "-q"], ["add", "-A"]] {
        let status = block_on(
            util::command::new_command("git")
                .arg("-C")
                .arg(repository)
                .args(arguments)
                .status(),
        )
        .expect("git runs");
        assert!(status.success(), "git {arguments:?}");
    }
}

fn search() -> GitSearch {
    GitSearch::locate(std::env::vars().collect()).expect("git is installed")
}

/// Three repositories: `orders` and `billing` each have their own `CodeMarkers.CheckoutRun` and
/// `Ledger.Post`, and `common` defines a marker that only `orders` uses.
fn fixture() -> (tempfile::TempDir, CodeResolver) {
    let root = tempfile::tempdir().expect("a temporary directory");

    let orders = root.path().join("orders");
    write(
        &orders,
        "src/Orders/CodeMarkers.cs",
        r#"namespace Contoso.Orders
{
    public static class CodeMarkers
    {
        /// <summary>The checkout run.</summary>
        [CodeMarkerName("Contoso.Orders.Checkout.Run")]
        public sealed class CheckoutRun : SingletonMonitoredScopeCodeMarker<CheckoutRun> { }

        public const string ChargeCard = "Orders-ChargeCard";

        [CodeMarkerName(codeMarker: WrappedName)]
        public sealed class WrappedMarker : SingletonMonitoredScopeCodeMarker<WrappedMarker> { }

        public const string WrappedName = "Orders-Wrapped";

        [CodeMarkerName("Orders-Traced")]
        public sealed class TracedMarker : SingletonMonitoredScopeCodeMarker<TracedMarker> { }
    }
}
"#,
    );
    write(
        &orders,
        "src/Orders/Checkout.cs",
        r#"using System.Threading.Tasks;

namespace Contoso.Orders
{
    public class Checkout : CheckoutBase
    {
        public async Task RunAsync(Cart cart)
        {
            await CodeMarkers.CheckoutRun.ExecuteAsync(async _ =>
            {
                this.Log("Starting checkout");
            });
        }

        public void Charge(Cart cart)
        {
            using (new MonitoredScope(CodeMarkers.ChargeCard))
            {
                this.gateway.Charge(cart);
            }
        }

        public void Wrapped()
        {
            CodeMarkers.WrappedMarker.Execute();
        }

        public void Direct()
        {
            using (new MonitoredScope("Direct-Literal"))
            {
                this.Work();
            }
        }

        public void Audit()
        {
            using (new MonitoredScope(CommonMarkers.Shared))
            {
                this.Work();
            }
        }

        public void Traced()
        {
            using (new MonitoredScope(CodeMarkers.TracedMarker.Instance.CodeMarkerName))
            {
                this.Work();
            }
        }
    }
}
"#,
    );
    write(
        &orders,
        "src/Orders/CheckoutBase.cs",
        "namespace Contoso.Orders\n{\n    public class CheckoutBase\n    {\n        public void Inherited()\n        {\n        }\n    }\n}\n",
    );
    write(
        &orders,
        "src/Orders/Ledger.cs",
        "namespace Contoso.Orders\n{\n    public class Ledger\n    {\n        public void Post(Entry entry)\n        {\n        }\n    }\n}\n",
    );
    write(
        &orders,
        "test/Orders.Tests/CheckoutTests.cs",
        "namespace Contoso.Orders.Tests\n{\n    public class CheckoutTests\n    {\n        public void RunsTheCheckout()\n        {\n            CodeMarkers.CheckoutRun.Execute();\n        }\n    }\n}\n",
    );
    track(&orders);

    let billing = root.path().join("billing");
    write(
        &billing,
        "src/Billing/CodeMarkers.cs",
        "namespace Contoso.Billing\n{\n    public static class CodeMarkers\n    {\n        [CodeMarkerName(\"Billing.Checkout.Run\")]\n        public sealed class CheckoutRun : SingletonMonitoredScopeCodeMarker<CheckoutRun> { }\n    }\n}\n",
    );
    write(
        &billing,
        "src/Billing/Invoice.cs",
        "namespace Contoso.Billing\n{\n    public class Invoice\n    {\n        public void Issue()\n        {\n            CodeMarkers.CheckoutRun.Execute();\n        }\n    }\n}\n",
    );
    write(
        &billing,
        "src/Billing/Ledger.cs",
        "namespace Contoso.Billing\n{\n    public class Ledger\n    {\n        public void Post(Entry entry)\n        {\n        }\n    }\n}\n",
    );
    track(&billing);

    let common = root.path().join("common");
    write(
        &common,
        "src/Common/CommonMarkers.cs",
        "namespace Contoso.Common\n{\n    public static class CommonMarkers\n    {\n        public const string Shared = \"Common-Shared\";\n    }\n}\n",
    );
    track(&common);

    let repositories = discover_repositories(&[root.path().to_path_buf()]);
    (root, CodeResolver::new(search(), repositories))
}

fn keys(
    marker: Option<&str>,
    message: Option<&str>,
    details: Option<&str>,
    process: Option<&str>,
) -> CodeKeys {
    CodeKeys::from_event(&EventText {
        marker,
        message,
        details,
        process,
    })
}

fn marker_keys(marker: &str) -> CodeKeys {
    keys(Some(marker), None, None, None)
}

fn of_kind(references: &[CodeRef], kind: CodeRefKind) -> Vec<&CodeRef> {
    references
        .iter()
        .filter(|reference| reference.kind == kind)
        .collect()
}

fn place(reference: &CodeRef) -> (&str, &str, Option<&str>) {
    (
        reference.repository.as_str(),
        reference.relative_path.as_str(),
        reference.enclosing.as_deref(),
    )
}

#[test]
fn a_marker_class_resolves_to_its_use_sites_best_first() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Contoso.Orders.Checkout.Run")));

    let uses = of_kind(&references, CodeRefKind::MarkerUse);
    assert_eq!(
        uses.iter()
            .map(|reference| place(reference))
            .collect::<Vec<_>>(),
        vec![
            ("orders", "src/Orders/Checkout.cs", Some("RunAsync")),
            (
                "orders",
                "test/Orders.Tests/CheckoutTests.cs",
                Some("RunsTheCheckout")
            ),
        ]
    );
    assert_eq!(uses.first().map(|reference| reference.line), Some(9));
    assert_eq!(
        uses.first().map(|reference| reference.symbol.as_str()),
        Some("CodeMarkers.CheckoutRun")
    );

    let definitions = of_kind(&references, CodeRefKind::MarkerDefinition);
    assert_eq!(definitions.len(), 1);
    assert_eq!(
        definitions
            .first()
            .map(|reference| (reference.relative_path.as_str(), reference.line)),
        Some(("src/Orders/CodeMarkers.cs", 7))
    );
}

#[test]
fn a_short_name_in_another_repository_is_not_mistaken_for_the_marker() {
    let (_root, resolver) = fixture();
    let billing = block_on(resolver.resolve(&marker_keys("Billing.Checkout.Run")));
    assert_eq!(
        of_kind(&billing, CodeRefKind::MarkerUse)
            .iter()
            .map(|reference| place(reference))
            .collect::<Vec<_>>(),
        vec![("billing", "src/Billing/Invoice.cs", Some("Issue"))]
    );
}

#[test]
fn a_string_constant_marker_resolves_to_where_it_is_passed() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Orders-ChargeCard")));
    assert_eq!(
        of_kind(&references, CodeRefKind::MarkerUse)
            .iter()
            .map(|reference| place(reference))
            .collect::<Vec<_>>(),
        vec![("orders", "src/Orders/Checkout.cs", Some("Charge"))]
    );
    assert_eq!(
        of_kind(&references, CodeRefKind::MarkerDefinition)
            .first()
            .map(|reference| reference.symbol.as_str()),
        Some("ChargeCard")
    );
}

#[test]
fn an_attribute_that_names_a_constant_is_followed_to_the_class() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Orders-Wrapped")));
    assert_eq!(
        of_kind(&references, CodeRefKind::MarkerUse)
            .iter()
            .map(|reference| place(reference))
            .collect::<Vec<_>>(),
        vec![("orders", "src/Orders/Checkout.cs", Some("Wrapped"))]
    );
}

#[test]
fn a_use_through_the_code_marker_name_property_is_a_use_not_an_attribute() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Orders-Traced")));
    assert_eq!(
        of_kind(&references, CodeRefKind::MarkerUse)
            .iter()
            .map(|reference| place(reference))
            .collect::<Vec<_>>(),
        vec![("orders", "src/Orders/Checkout.cs", Some("Traced"))]
    );
}

#[test]
fn a_literal_passed_straight_to_a_scope_is_a_use() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Direct-Literal")));
    assert_eq!(
        references.iter().map(place).collect::<Vec<_>>(),
        vec![("orders", "src/Orders/Checkout.cs", Some("Direct"))]
    );
    assert_eq!(
        references.first().map(|reference| reference.kind),
        Some(CodeRefKind::MarkerUse)
    );
}

#[test]
fn a_marker_declared_in_one_repository_is_found_in_the_use_of_another() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Common-Shared")));
    assert_eq!(
        of_kind(&references, CodeRefKind::MarkerUse)
            .iter()
            .map(|reference| place(reference))
            .collect::<Vec<_>>(),
        vec![("orders", "src/Orders/Checkout.cs", Some("Audit"))]
    );
    assert_eq!(
        of_kind(&references, CodeRefKind::MarkerDefinition)
            .iter()
            .map(|reference| reference.repository.as_str())
            .collect::<Vec<_>>(),
        vec!["common"]
    );
}

#[test]
fn prefetching_markers_resolves_them_the_same_way() {
    let (_root, resolver) = fixture();
    let markers: Vec<String> = [
        "Orders-ChargeCard",
        "Direct-Literal",
        "Orders-ChargeCard",
        "Nowhere-Defined",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    block_on(resolver.prefetch_markers(&markers));
    let references = block_on(resolver.resolve(&marker_keys("Orders-ChargeCard")));
    assert_eq!(of_kind(&references, CodeRefKind::MarkerUse).len(), 1);
    assert!(block_on(resolver.resolve(&marker_keys("Nowhere-Defined"))).is_empty());
}

#[test]
fn a_class_and_method_prefix_resolves_to_the_method() {
    let (_root, resolver) = fixture();
    let references =
        block_on(resolver.resolve(&keys(None, Some("Checkout.Charge: charging"), None, None)));
    assert_eq!(
        references
            .iter()
            .map(|reference| (place(reference), reference.kind, reference.origin))
            .collect::<Vec<_>>(),
        vec![(
            ("orders", "src/Orders/Checkout.cs", None),
            CodeRefKind::Method,
            KeyOrigin::Prefix
        )]
    );
    assert_eq!(references.first().map(|reference| reference.line), Some(15));
}

#[test]
fn a_repository_named_like_the_process_is_searched_first() {
    let (_root, resolver) = fixture();
    let repositories_for = |message: &str, process: &str| -> Vec<String> {
        block_on(resolver.resolve(&keys(None, Some(message), None, Some(process))))
            .into_iter()
            .map(|reference| reference.repository)
            .collect()
    };
    assert_eq!(
        repositories_for("Ledger.Post: writing", "Contoso.Billing.Host"),
        ["billing"]
    );
    assert_eq!(
        repositories_for("Ledger.Post: writing", "Contoso.Orders.Host"),
        ["orders"]
    );
    // Both are searched when the process names neither.
    assert_eq!(
        repositories_for("Ledger.Post: writing", "Contoso.Host"),
        ["billing", "orders"]
    );
    // The others are searched when the class is not where the process says.
    assert_eq!(
        repositories_for("Checkout.Charge: charging", "Contoso.Billing.Host"),
        ["orders"]
    );
}

#[test]
fn an_inherited_method_resolves_to_the_base_class() {
    let (_root, resolver) = fixture();
    let references =
        block_on(resolver.resolve(&keys(None, Some("Checkout.Inherited done"), None, None)));
    assert_eq!(
        references
            .iter()
            .map(|reference| (place(reference), reference.kind, reference.symbol.as_str()))
            .collect::<Vec<_>>(),
        vec![(
            ("orders", "src/Orders/CheckoutBase.cs", None),
            CodeRefKind::Method,
            "CheckoutBase.Inherited"
        )]
    );
    assert_eq!(references.first().map(|reference| reference.line), Some(5));
}

#[test]
fn a_method_that_is_declared_nowhere_gives_the_type() {
    let (_root, resolver) = fixture();
    let references =
        block_on(resolver.resolve(&keys(None, Some("Checkout.Vanished done"), None, None)));
    assert_eq!(
        references
            .iter()
            .map(|reference| (place(reference), reference.kind))
            .collect::<Vec<_>>(),
        vec![(
            ("orders", "src/Orders/Checkout.cs", None),
            CodeRefKind::Type
        )]
    );
    assert_eq!(references.first().map(|reference| reference.line), Some(5));
}

#[test]
fn a_dotted_marker_with_no_declaration_is_read_as_a_method() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Contoso.Orders.Checkout.Charge")));
    assert_eq!(
        references
            .iter()
            .map(|reference| (place(reference), reference.kind, reference.origin))
            .collect::<Vec<_>>(),
        vec![(
            ("orders", "src/Orders/Checkout.cs", None),
            CodeRefKind::Method,
            KeyOrigin::Marker
        )]
    );
}

#[test]
fn the_frames_of_a_call_stack_resolve_in_order() {
    let (_root, resolver) = fixture();
    let stack = "   at System.Threading.Tasks.Task.Wait()\n   at Contoso.Orders.Checkout.Charge(Cart cart) in C:\\__w\\1\\s\\Checkout.cs:line 17\n   at Contoso.Orders.Checkout.<RunAsync>d__3.MoveNext()";
    let references = block_on(resolver.resolve(&keys(None, None, Some(stack), None)));
    assert_eq!(
        references
            .iter()
            .map(|reference| (reference.symbol.as_str(), reference.origin, reference.line))
            .collect::<Vec<_>>(),
        vec![
            ("Checkout.Charge", KeyOrigin::Frame, 15),
            ("Checkout.RunAsync", KeyOrigin::Frame, 7),
        ]
    );
}

#[test]
fn something_that_is_in_no_repository_resolves_to_nothing() {
    let (_root, resolver) = fixture();
    let keys = keys(
        Some("Not-Anywhere"),
        Some("Missing.Thing happened"),
        Some("at Nowhere.Land.Go()"),
        None,
    );
    assert!(block_on(resolver.resolve(&keys)).is_empty());
}

#[test]
fn clearing_the_cache_sees_a_changed_checkout() {
    let (root, resolver) = fixture();
    assert!(block_on(resolver.resolve(&marker_keys("Later-Added"))).is_empty());
    write(
        &root.path().join("billing"),
        "src/Billing/Later.cs",
        "namespace Contoso.Billing\n{\n    public class Later\n    {\n        public void Go()\n        {\n            using (new MonitoredScope(\"Later-Added\"))\n            {\n            }\n        }\n    }\n}\n",
    );
    track(&root.path().join("billing"));
    assert!(block_on(resolver.resolve(&marker_keys("Later-Added"))).is_empty());
    resolver.clear_cache();
    assert_eq!(
        block_on(resolver.resolve(&marker_keys("Later-Added"))).len(),
        1
    );
}

/// Resolves everything a real trace names against real checkouts and reports how much was found.
///
/// `cargo test -p kusto_code --test local_repositories -- --ignored --nocapture`, with the
/// environment variables `KUSTO_CODE_ROOT` (default `/Users/gregm/Projects/mwc`) and
/// `KUSTO_CODE_SAMPLE` (default `fork-docs/samples/sample2.ktt` of this repository).
#[test]
#[ignore = "needs the local repositories and a real trace"]
fn resolves_a_real_trace_against_real_repositories() {
    let root =
        std::env::var("KUSTO_CODE_ROOT").unwrap_or_else(|_| "/Users/gregm/Projects/mwc".into());
    let sample = std::env::var("KUSTO_CODE_SAMPLE").unwrap_or_else(|_| {
        format!(
            "{}/../../fork-docs/samples/sample2.ktt",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let repositories = discover_repositories(&[root.into()]);
    println!("{} repositories", repositories.len());
    let resolver = CodeResolver::new(search(), repositories);

    let text = fs::read_to_string(&sample).expect("the sample trace");
    let trace: serde_json::Value = serde_json::from_str(&text).expect("a .ktt file");
    let table = &trace["tables"][0];
    let column = |name: &str| {
        table["columns"]
            .as_array()
            .and_then(|columns| columns.iter().position(|column| column["name"] == name))
    };
    let (marker_column, message_column, details_column, process_column) = (
        column("MarkerName"),
        column("MessageText"),
        column("CustomData"),
        column("ProcessName"),
    );
    let cell = |row: &serde_json::Value, column: Option<usize>| -> Option<String> {
        let value = &row[column?];
        Some(
            value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_string),
        )
    };

    let mut events = Vec::new();
    for row in table["rows"].as_array().expect("rows") {
        let (marker, message, details, process) = (
            cell(row, marker_column),
            cell(row, message_column),
            cell(row, details_column),
            cell(row, process_column),
        );
        events.push(keys(
            marker.as_deref(),
            message.as_deref(),
            details.as_deref(),
            process.as_deref(),
        ));
    }

    let markers: Vec<String> = {
        let mut distinct: Vec<String> = events
            .iter()
            .filter_map(|keys| keys.marker.clone())
            .collect();
        distinct.sort();
        distinct.dedup();
        distinct
    };
    let started = std::time::Instant::now();
    block_on(resolver.prefetch_markers(&markers));
    println!(
        "prefetched {} markers in {:?}",
        markers.len(),
        started.elapsed()
    );

    let started = std::time::Instant::now();
    let mut marker_found: HashMap<String, usize> = HashMap::new();
    for marker in &markers {
        let references = block_on(resolver.resolve(&marker_keys(marker)));
        marker_found.insert(
            marker.clone(),
            of_kind(&references, CodeRefKind::MarkerUse).len(),
        );
    }
    let with_use = marker_found.values().filter(|count| **count > 0).count();
    println!(
        "markers with a use site: {with_use} of {} ({:?})",
        markers.len(),
        started.elapsed()
    );
    for (marker, count) in &marker_found {
        if *count == 0 {
            println!("  no use site: {marker}");
        }
    }

    let mut prefixes: Vec<_> = events
        .iter()
        .filter_map(|keys| {
            keys.prefix
                .clone()
                .map(|prefix| (prefix, keys.process.clone()))
        })
        .collect();
    prefixes.sort_by_key(|(prefix, process)| (prefix.full_name(), process.clone()));
    prefixes.dedup();
    let started = std::time::Instant::now();
    let (mut methods, mut types) = (0, 0);
    for (prefix, process) in &prefixes {
        let keys = CodeKeys {
            prefix: Some(prefix.clone()),
            process: process.clone(),
            ..CodeKeys::default()
        };
        let references = block_on(resolver.resolve(&keys));
        if references
            .iter()
            .any(|reference| reference.kind == CodeRefKind::Method)
        {
            methods += 1;
        } else if !references.is_empty() {
            types += 1;
            println!("  type only: {}", prefix.full_name());
        } else {
            println!("  not found: {}", prefix.full_name());
        }
    }
    println!(
        "prefixes of {} distinct: {methods} to a method, {types} to a type only ({:?})",
        prefixes.len(),
        started.elapsed()
    );
}
