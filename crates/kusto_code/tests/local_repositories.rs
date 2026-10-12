use std::collections::HashMap;
use std::fs;
use std::path::Path;

use futures::executor::block_on;
use kusto_code::{
    CodeKeys, CodeRef, CodeRefKind, CodeResolver, EventText, GitSearch, KeyOrigin, SymbolPath,
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
    GitSearch::locate(std::env::vars()).expect("git is installed")
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
    write(
        &orders,
        "src/Orders/Cache.cs",
        r#"namespace Contoso.Orders
{
    public class Cache
    {
        public void Initialize()
        {
            if (this.ready)
            {
                this.logger.LogInfo("Memory backed cloud cache already initialized. Taking fast path.");
                return;
            }
        }

        public void Fetch(Locator locator)
        {
            this.logger.LogInfo($"Successfully fetched cluster details for {locator}");
        }

        /// Taking the slow path for the unknown cluster.
        public void Slow()
        {
        }
    }
}
"#,
    );
    write(
        &orders,
        "src/Orders/AnotherCache.cs",
        "namespace Contoso.Orders\n{\n    public class AnotherCache\n    {\n        public void Fetch()\n        {\n            this.logger.LogInfo($\"Successfully fetched cluster details for {this.name}\");\n        }\n    }\n}\n",
    );
    let busy: String = (0..20)
        .map(|index| format!("        public void Step{index}()\n        {{\n            this.logger.LogInfo(\"Started processing the request\");\n        }}\n"))
        .collect();
    write(
        &orders,
        "src/Orders/Busy.cs",
        &format!("namespace Contoso.Orders\n{{\n    public class Busy\n    {{\n{busy}    }}\n}}\n"),
    );
    write(
        &orders,
        "src/Orders/Forwarder.cs",
        "namespace Contoso.Orders\n{\n    public class Forwarder\n    {\n        public async Task<int> ForwardAsync()\n        {\n#if NETFRAMEWORK\n            this.Legacy = true;\n#else\n            this.Legacy = false;\n#endif\n            async Task<int> WithTimeout()\n            {\n                return await this.Send();\n            }\n\n            return await WithTimeout();\n        }\n    }\n}\n",
    );
    write(
        &orders,
        "src/Orders/Warm.cs",
        "namespace Contoso.Orders\n{\n    public class Warm\n    {\n        public void Use()\n        {\n            this.logger.LogInfo(\"Warm cache ready for use\");\n        }\n    }\n}\n",
    );
    write(
        &orders,
        "src/Orders/Posting.cs",
        "namespace Contoso.Orders\n{\n    public class Posting\n    {\n        public void Post()\n        {\n            this.logger.LogInfo(\"Ledger posted the entry successfully\");\n        }\n    }\n}\n",
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
    write(
        &billing,
        "src/Billing/Posting.cs",
        "namespace Contoso.Billing\n{\n    public class Posting\n    {\n        public void Post()\n        {\n            this.logger.LogInfo(\"Ledger posted the entry successfully\");\n        }\n    }\n}\n",
    );
    track(&billing);

    let common = root.path().join("common");
    write(
        &common,
        "src/Common/CommonMarkers.cs",
        "namespace Contoso.Common\n{\n    public static class CommonMarkers\n    {\n        public const string Shared = \"Common-Shared\";\n    }\n}\n",
    );
    write(
        &common,
        "src/Common/Warm.cs",
        "namespace Contoso.Common\n{\n    public class Warmup\n    {\n        public void Start()\n        {\n            this.logger.LogInfo(\"Warm cache ready for the first request now\");\n        }\n    }\n}\n",
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

fn message_keys(message: &str, process: Option<&str>) -> CodeKeys {
    keys(None, Some(message), None, process)
}

#[test]
fn a_message_with_nothing_filled_in_is_found_where_it_is_logged() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&message_keys(
        "Memory backed cloud cache already initialized. Taking fast path.",
        None,
    )));
    assert_eq!(
        references
            .iter()
            .map(|reference| (
                place(reference),
                reference.kind,
                reference.origin,
                reference.line
            ))
            .collect::<Vec<_>>(),
        vec![(
            ("orders", "src/Orders/Cache.cs", Some("Initialize")),
            CodeRefKind::LogLine,
            KeyOrigin::Message,
            9
        )]
    );
    assert!(
        references
            .first()
            .is_some_and(|reference| reference.text.starts_with("this.logger.LogInfo("))
    );
}

#[test]
fn a_message_with_values_filled_in_is_found_by_the_text_before_them() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&message_keys(
        "Successfully fetched cluster details for [MetadataLocator: [TenantId=0d9fca32-3db8-4ce8-a52e-1c9557f46ebf, RealmId=, WorkspaceId=84980b9c-4cb0-40c2-a152-b70d5fca1109, CapacityId=]]",
        None,
    )));
    assert_eq!(
        references
            .iter()
            .map(|reference| (place(reference), reference.line))
            .collect::<Vec<_>>(),
        vec![
            (("orders", "src/Orders/AnotherCache.cs", Some("Fetch")), 7),
            (("orders", "src/Orders/Cache.cs", Some("Fetch")), 16),
        ]
    );
    assert!(
        references
            .iter()
            .all(|reference| reference.symbol == "Successfully fetched cluster details for")
    );
}

#[test]
fn a_line_with_only_part_of_the_message_does_not_end_the_search() {
    let (_root, resolver) = fixture();
    // `orders` is named like the process and holds "Warm cache ready for", but the whole text
    // is only in `common`.
    let references = block_on(resolver.resolve(&message_keys(
        "Warm cache ready for the first request now",
        Some("Contoso.Orders.Host"),
    )));
    assert_eq!(
        references.iter().map(place).collect::<Vec<_>>(),
        vec![("common", "src/Common/Warm.cs", Some("Start"))]
    );
}

#[test]
fn a_message_that_several_lines_hold_says_how_many() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&message_keys(
        "Successfully fetched cluster details for the cluster",
        None,
    )));
    assert_eq!(
        references
            .iter()
            .map(|reference| reference.matches)
            .collect::<Vec<_>>(),
        vec![2, 2]
    );
    let alone = block_on(resolver.resolve(&message_keys(
        "Memory backed cloud cache already initialized. Taking fast path.",
        None,
    )));
    assert_eq!(alone.first().map(|reference| reference.matches), Some(1));
}

#[test]
fn the_class_and_method_a_message_starts_with_are_not_part_of_what_is_looked_for() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&message_keys(
        "Cache.Initialize: Memory backed cloud cache already initialized. Taking fast path.",
        None,
    )));
    let kinds: Vec<(CodeRefKind, usize)> = references
        .iter()
        .map(|reference| (reference.kind, reference.line))
        .collect();
    // The log line comes first, then the method the prefix names.
    assert_eq!(
        kinds,
        vec![(CodeRefKind::LogLine, 9), (CodeRefKind::Method, 5)]
    );
}

#[test]
fn a_log_line_in_a_file_the_other_lookups_found_comes_first() {
    let (_root, resolver) = fixture();
    let message = "Successfully fetched cluster details for the cluster";
    let without = block_on(resolver.resolve(&message_keys(message, None)));
    assert_eq!(
        without
            .first()
            .map(|reference| reference.relative_path.as_str()),
        Some("src/Orders/AnotherCache.cs")
    );
    let with_prefix =
        block_on(resolver.resolve(&message_keys(&format!("Cache.Fetch: {message}"), None)));
    assert_eq!(
        with_prefix
            .first()
            .map(|reference| reference.relative_path.as_str()),
        Some("src/Orders/Cache.cs")
    );
}

#[test]
fn text_so_common_that_it_places_nothing_is_left_alone() {
    let (_root, resolver) = fixture();
    assert!(
        block_on(resolver.resolve(&message_keys("Started processing the request", None)))
            .is_empty()
    );
}

#[test]
fn a_comment_or_a_line_without_a_string_is_not_a_log_line() {
    let (_root, resolver) = fixture();
    assert!(
        block_on(resolver.resolve(&message_keys(
            "Taking the slow path for the unknown cluster",
            None
        )))
        .is_empty()
    );
}

#[test]
fn a_repository_named_like_the_process_is_searched_first_for_a_message() {
    let (_root, resolver) = fixture();
    let repositories_for = |process: Option<&str>| -> Vec<String> {
        block_on(resolver.resolve(&message_keys(
            "Ledger posted the entry successfully",
            process,
        )))
        .into_iter()
        .map(|reference| reference.repository)
        .collect()
    };
    assert_eq!(repositories_for(Some("Contoso.Billing.Host")), ["billing"]);
    assert_eq!(repositories_for(Some("Contoso.Orders.Host")), ["orders"]);
    assert_eq!(repositories_for(None), ["billing", "orders"]);
}

fn source_file(
    resolver: &CodeResolver,
    file_name: &str,
    line: usize,
    symbol: &str,
    process: Option<&str>,
) -> Vec<CodeRef> {
    block_on(resolver.find_source_file(
        file_name,
        line,
        SymbolPath::parse(symbol).as_ref(),
        process,
    ))
}

#[test]
fn a_frame_opens_at_its_line_in_the_file_it_names() {
    let (_root, resolver) = fixture();
    let charge = "Contoso.Orders.Checkout.Charge";
    for file_name in [
        "Checkout.cs",
        "CHECKOUT.CS",
        r"C:\__w\1\s\src\Orders\Checkout.cs",
        "/agent/build/src/Orders/Checkout.cs",
    ] {
        let found = source_file(&resolver, file_name, 17, charge, None);
        assert_eq!(
            found
                .iter()
                .map(|reference| (
                    place(reference),
                    reference.line,
                    reference.kind,
                    reference.origin
                ))
                .collect::<Vec<_>>(),
            vec![(
                ("orders", "src/Orders/Checkout.cs", Some("Charge")),
                17,
                CodeRefKind::StackLine,
                KeyOrigin::Frame
            )],
            "{file_name}"
        );
        assert!(found[0].text.starts_with("using (new MonitoredScope"));
    }
}

#[test]
fn a_line_that_is_not_in_the_frames_method_opens_the_method_instead() {
    let (_root, resolver) = fixture();
    let charge = "Contoso.Orders.Checkout.Charge";
    // The checkout has moved on from the build: line 9 is in `RunAsync` here, line 500 is past the end.
    for stale_line in [9, 5, 500] {
        let found = source_file(&resolver, "Checkout.cs", stale_line, charge, None);
        assert_eq!(
            found
                .first()
                .map(|reference| (reference.line, reference.text.as_str())),
            Some((15, "public void Charge(Cart cart)")),
            "line {stale_line}"
        );
    }
    // Without a method to check against the line is kept, up to the end of the file.
    let found = source_file(&resolver, "Checkout.cs", 500, "lower.case", None);
    assert!(
        found
            .first()
            .is_some_and(|reference| reference.line < 500 && reference.line > 40)
    );
}

#[test]
fn a_frame_after_preprocessor_lines_or_in_a_local_function_keeps_its_line() {
    let (_root, resolver) = fixture();
    let forward = "Contoso.Orders.Forwarder.ForwardAsync";
    // Line 10 is after `#if`/`#else`, and line 14 is in the local function `WithTimeout`.
    for line in [10, 14, 17] {
        let found = source_file(&resolver, "Forwarder.cs", line, forward, None);
        assert_eq!(
            found.first().map(|reference| reference.line),
            Some(line),
            "line {line}"
        );
    }
}

#[test]
fn files_of_one_name_are_ordered_by_the_frames_namespace_and_the_process() {
    let (_root, resolver) = fixture();
    let repositories_of = |symbol: &str, process: Option<&str>| -> Vec<String> {
        source_file(&resolver, "Ledger.cs", 6, symbol, process)
            .into_iter()
            .map(|reference| reference.repository)
            .collect()
    };
    assert_eq!(
        repositories_of("Contoso.Billing.Ledger.Post", None),
        ["billing", "orders"]
    );
    assert_eq!(
        repositories_of("Contoso.Orders.Ledger.Post", None),
        ["orders", "billing"]
    );
    assert_eq!(
        repositories_of("Ledger.Post", Some("Contoso.Billing.Host")),
        ["billing", "orders"]
    );
}

#[test]
fn a_file_in_no_checkout_gives_nothing() {
    let (_root, resolver) = fixture();
    assert!(
        source_file(
            &resolver,
            "Missing.cs",
            3,
            "Contoso.Orders.Missing.Go",
            None
        )
        .is_empty()
    );
}

#[test]
fn the_list_of_files_is_read_again_after_the_cache_is_cleared() {
    let (root, resolver) = fixture();
    assert!(source_file(&resolver, "Later.cs", 7, "Contoso.Billing.Later.Go", None).is_empty());
    write(
        &root.path().join("billing"),
        "src/Billing/Later.cs",
        "namespace Contoso.Billing\n{\n    public class Later\n    {\n        public void Go()\n        {\n            this.Work();\n        }\n    }\n}\n",
    );
    track(&root.path().join("billing"));
    assert!(source_file(&resolver, "Later.cs", 7, "Contoso.Billing.Later.Go", None).is_empty());
    resolver.clear_cache();
    assert_eq!(
        source_file(&resolver, "Later.cs", 7, "Contoso.Billing.Later.Go", None).len(),
        1
    );
}

#[test]
fn a_reference_carries_the_text_of_its_line() {
    let (_root, resolver) = fixture();
    let references = block_on(resolver.resolve(&marker_keys("Contoso.Orders.Checkout.Run")));
    let texts: Vec<(CodeRefKind, &str)> = references
        .iter()
        .map(|reference| (reference.kind, reference.text.as_str()))
        .collect();
    assert_eq!(
        texts.first(),
        Some(&(
            CodeRefKind::MarkerUse,
            "await CodeMarkers.CheckoutRun.ExecuteAsync(async _ =>"
        ))
    );
    // The definition is reported at the class, not at the attribute above it.
    assert_eq!(
        texts.last(),
        Some(&(
            CodeRefKind::MarkerDefinition,
            "public sealed class CheckoutRun : SingletonMonitoredScopeCodeMarker<CheckoutRun> { }"
        ))
    );
}

#[test]
fn what_was_found_is_looked_up_again_once_it_is_old() {
    let (root, resolver) = fixture();
    let resolver = resolver.with_cache_lifetime(std::time::Duration::ZERO);
    assert!(block_on(resolver.resolve(&marker_keys("Later-Added"))).is_empty());
    write(
        &root.path().join("billing"),
        "src/Billing/Later.cs",
        "namespace Contoso.Billing\n{\n    public class Later\n    {\n        public void Go()\n        {\n            using (new MonitoredScope(\"Later-Added\"))\n            {\n            }\n        }\n    }\n}\n",
    );
    track(&root.path().join("billing"));
    assert_eq!(
        block_on(resolver.resolve(&marker_keys("Later-Added"))).len(),
        1
    );
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

    let mut by_text: std::collections::BTreeMap<String, CodeKeys> =
        std::collections::BTreeMap::new();
    for event in &events {
        if let Some(first) = event.literals.first().and_then(|pieces| pieces.first()) {
            by_text
                .entry(first.clone())
                .or_insert_with(|| event.clone());
        }
    }
    let step = (by_text.len() / 100).max(1);
    let sampled: Vec<&CodeKeys> = by_text.values().step_by(step).collect();
    let started = std::time::Instant::now();
    let (mut placed, mut too_common_or_absent) = (0, 0);
    for event in &sampled {
        let keys = CodeKeys {
            literals: event.literals.clone(),
            process: event.process.clone(),
            ..CodeKeys::default()
        };
        let references = block_on(resolver.resolve(&keys));
        if references.is_empty() {
            too_common_or_absent += 1;
            let shown: Vec<&str> = event
                .literals
                .iter()
                .filter_map(|pieces| pieces.first().map(String::as_str))
                .collect();
            println!("  not placed: {shown:?}");
        } else {
            placed += 1;
            if placed <= 12 {
                if let Some(reference) = references.first() {
                    println!(
                        "  logged at {}:{} for {:?}",
                        reference.relative_path, reference.line, reference.symbol
                    );
                }
            }
        }
    }
    println!(
        "messages: {} distinct texts, {} sampled: {placed} placed, {too_common_or_absent} not ({:?})",
        by_text.len(),
        sampled.len(),
        started.elapsed()
    );
}
