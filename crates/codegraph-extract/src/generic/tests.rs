use crate::extract_file;
use codegraph_core::{ExtractionResult, Language, NodeKind, ReferenceKind};

fn names(r: &ExtractionResult, kind: NodeKind) -> Vec<String> {
    r.nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.qualified_name.clone())
        .collect()
}

fn refs(r: &ExtractionResult, kind: ReferenceKind) -> Vec<String> {
    r.unresolved_references
        .iter()
        .filter(|u| u.reference_kind == kind)
        .map(|u| u.reference_name.clone())
        .collect()
}

/// Which declaration a reference comes from.
fn from(r: &ExtractionResult, name: &str) -> String {
    let u = r
        .unresolved_references
        .iter()
        .find(|u| u.reference_name == name)
        .unwrap_or_else(|| panic!("no ref {name}"));
    r.nodes
        .iter()
        .find(|n| n.id == u.from_node_id)
        .map(|n| n.name.clone())
        .unwrap_or_default()
}

#[test]
fn solidity_contracts() {
    let src = r#"import "./Ownable.sol";
contract Token is Ownable, IERC20 {
    uint256 public total;
    event Transfer(address to, uint256 v);
    enum State { Open, Closed }
    modifier onlyAdmin() { _; }
    constructor(uint256 s) { total = s; }
    function send(address to, uint256 v) external onlyAdmin returns (bool) {
        emit Transfer(to, v);
        SafeMath.add(v, 1);
        return true;
    }
    receive() external payable {}
}
library SafeMath { function add(uint a, uint b) internal pure returns (uint) { return a + b; } }
"#;
    let r = extract_file("t.sol", src, Language::Solidity);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(names(&r, NodeKind::Class), vec!["Token", "SafeMath"]);
    assert_eq!(
        names(&r, NodeKind::Method),
        vec![
            "Token::onlyAdmin",
            "Token::constructor",
            "Token::send",
            "Token::receive",
            "SafeMath::add"
        ]
    );
    assert_eq!(
        names(&r, NodeKind::EnumMember),
        vec!["Token::State::Open", "Token::State::Closed"]
    );
    assert_eq!(refs(&r, ReferenceKind::Extends), vec!["Ownable", "IERC20"]);
    assert_eq!(refs(&r, ReferenceKind::Imports), vec!["./Ownable.sol"]);
    assert_eq!(
        refs(&r, ReferenceKind::Calls),
        vec!["onlyAdmin", "Transfer", "SafeMath.add"]
    );
    assert_eq!(from(&r, "SafeMath.add"), "send");
    let send = r.nodes.iter().find(|n| n.name == "send").unwrap();
    assert_eq!(
        send.signature.as_deref(),
        Some("(address to, uint256 v) external returns (bool)")
    );
}

#[test]
fn pascal_units() {
    let src = "unit Shapes;\ninterface\nuses SysUtils, Geometry.Points;\ntype\n  TShape = class sealed(TObject, IDrawable)\n  public\n    procedure Draw;\n    function Area: Double; virtual; abstract;\n  end;\n  TColor = (clRed, clGreen);\nimplementation\nprocedure TShape.Draw;\nbegin\n  WriteLn(Format('%s', [Name]));\n  Refresh;\n  inherited Draw;\nend;\nfunction Helper(A: Integer): Integer;\nbegin\n  for var I in List do Use(I);\n  Result := A;\nend;\nend.\n";
    let r = extract_file("Shapes.pas", src, Language::Pascal);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(names(&r, NodeKind::Class), vec!["TShape"]);
    // Draw is defined in the implementation; Area only declared.
    assert_eq!(
        names(&r, NodeKind::Method),
        vec!["TShape::Area", "TShape::Draw"]
    );
    assert_eq!(names(&r, NodeKind::Function), vec!["Helper"]);
    assert_eq!(
        names(&r, NodeKind::EnumMember),
        vec!["TColor::clRed", "TColor::clGreen"]
    );
    assert_eq!(
        refs(&r, ReferenceKind::Imports),
        vec!["SysUtils", "Geometry.Points"]
    );
    assert_eq!(refs(&r, ReferenceKind::Extends), vec!["TObject"]);
    assert_eq!(refs(&r, ReferenceKind::Implements), vec!["IDrawable"]);
    let calls = refs(&r, ReferenceKind::Calls);
    for c in ["WriteLn", "Format", "Refresh", "Draw", "Use"] {
        assert!(calls.contains(&c.to_string()), "{c} in {calls:?}");
    }
    assert_eq!(from(&r, "Refresh"), "Draw");
}

#[test]
fn terraform_blocks() {
    let src = "variable \"region\" {}\nlocals {\n  name = \"app-${var.region}\"\n}\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = local.name\n}\nmodule \"vpc\" {\n  source = \"./modules/vpc\"\n  cidr   = var.region\n}\noutput \"arn\" {\n  value = aws_s3_bucket.logs.arn\n  id    = module.vpc.id\n  each  = each.key\n}\n";
    let r = extract_file("main.tf", src, Language::Terraform);
    assert_eq!(names(&r, NodeKind::Struct), vec!["aws_s3_bucket::logs"]);
    assert_eq!(names(&r, NodeKind::Variable), vec!["var::region"]);
    assert_eq!(
        names(&r, NodeKind::Constant),
        vec!["local::name", "output::arn"]
    );
    assert_eq!(names(&r, NodeKind::Module), vec!["module::vpc"]);
    assert_eq!(
        refs(&r, ReferenceKind::Imports),
        vec!["./modules/vpc/main.tf"]
    );
    assert_eq!(
        refs(&r, ReferenceKind::References),
        vec![
            "var.region",
            "local.name",
            "var.region",
            "aws_s3_bucket.logs",
            "module.vpc"
        ]
    );
    assert_eq!(from(&r, "aws_s3_bucket.logs"), "arn");
}

#[test]
fn nix_bindings() {
    let src = "{ lib, pkgs, ... }:\nlet\n  helper = x: y: x + y;\nin {\n  imports = [ ./hardware.nix ];\n  config = lib.mkIf true {\n    pkgs.foo = pkgs.callPackage ./pkg.nix { };\n    x = helper 1 2;\n    y = import ./other.nix { };\n  };\n}\n";
    let r = extract_file("default.nix", src, Language::Nix);
    assert_eq!(names(&r, NodeKind::Function), vec!["helper"]);
    assert!(names(&r, NodeKind::Variable).contains(&"config".to_string()));
    assert_eq!(
        refs(&r, ReferenceKind::Imports),
        vec!["./hardware.nix", "./pkg.nix", "./other.nix"]
    );
    assert_eq!(
        refs(&r, ReferenceKind::Calls),
        vec!["lib.mkIf", "pkgs.callPackage", "helper"]
    );
    let config = r.nodes.iter().find(|n| n.name == "config").unwrap();
    assert!(config.is_exported);
    let helper = r.nodes.iter().find(|n| n.name == "helper").unwrap();
    assert!(!helper.is_exported, "a let binding is not returned");
}

#[test]
fn erlang_modules() {
    let src = "-module(shop).\n-behaviour(gen_server).\n-export([add/2]).\n-record(item, {name}).\nadd(X, Y) when X > 0 ->\n    lists:map(fun inc/1, [X]),\n    gen_server:call(?MODULE, ping),\n    #item{name = X};\nadd(_, Y) -> helper(Y).\nhelper(N) -> ?MODULE:add(N, 1).\ninc(X) -> X + 1.\n";
    let r = extract_file("shop.erl", src, Language::Erlang);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        names(&r, NodeKind::Function),
        vec!["shop::add/2", "shop::helper/1", "shop::inc/1"]
    );
    let add = r.nodes.iter().find(|n| n.name == "add").unwrap();
    assert_eq!((add.start_line, add.end_line), (5, 9), "clauses merge");
    assert!(add.is_exported);
    assert!(
        !r.nodes
            .iter()
            .find(|n| n.name == "inc")
            .unwrap()
            .is_exported
    );
    assert_eq!(refs(&r, ReferenceKind::Implements), vec!["gen_server"]);
    assert_eq!(
        refs(&r, ReferenceKind::Calls),
        vec![
            "lists::map/2",
            "gen_server::call/2",
            "shop::handle_call/3",
            "helper/1",
            "add/2"
        ]
    );
    assert_eq!(refs(&r, ReferenceKind::References), vec!["inc/1", "item"]);
    // The pair before the argument list is the MFA, not the node name.
    let rpc = "-module(c).\nrun() -> rpc:call('n@host', worker, start, [1]).\n";
    let r = extract_file("c.erl", rpc, Language::Erlang);
    assert_eq!(
        refs(&r, ReferenceKind::Calls),
        vec!["rpc::call/4", "worker::start/1"]
    );
}

#[test]
fn objc_classes() {
    let src = "#import \"Cache.h\"\nNS_ASSUME_NONNULL_BEGIN\n@interface ImageCache : NSObject <Loader>\n@property (nonatomic) NSString *name;\n@end\nNS_ASSUME_NONNULL_END\n@implementation ImageCache\n+ (instancetype)shared { return [[ImageCache alloc] init]; }\n- (void)store:(id)img forKey:(NSString *)key {\n    [self load:key];\n    helper(1);\n}\n@end\ntypedef NS_ENUM(NSInteger, Mode) { ModeA };\n";
    let r = extract_file("Cache.m", src, Language::ObjC);
    assert_eq!(names(&r, NodeKind::Class), vec!["ImageCache"]);
    assert_eq!(names(&r, NodeKind::Property), vec!["ImageCache::name"]);
    assert_eq!(
        names(&r, NodeKind::Method),
        vec!["ImageCache::shared", "ImageCache::store:forKey:"]
    );
    assert_eq!(names(&r, NodeKind::Enum), vec!["Mode"]);
    assert_eq!(refs(&r, ReferenceKind::Extends), vec!["NSObject"]);
    assert_eq!(refs(&r, ReferenceKind::Implements), vec!["Loader"]);
    let calls = refs(&r, ReferenceKind::Calls);
    assert!(calls.contains(&"ImageCache.alloc".to_string()), "{calls:?}");
    assert!(calls.contains(&"load:".to_string()));
    assert!(calls.contains(&"helper".to_string()));
}

#[test]
fn cfml_components() {
    let script = "component extends=\"models.Base\" {\n  function init() {\n    dao = createObject(\"component\", \"models.UserDAO\");\n    return this;\n  }\n  function list() { return dao.search(1); }\n}\n";
    let r = extract_file("UserService.cfc", script, Language::Cfml);
    assert_eq!(names(&r, NodeKind::Class), vec!["UserService"]);
    assert_eq!(
        names(&r, NodeKind::Method),
        vec!["UserService::init", "UserService::list"]
    );
    assert_eq!(refs(&r, ReferenceKind::Extends), vec!["Base"]);
    assert_eq!(refs(&r, ReferenceKind::Instantiates), vec!["UserDAO"]);
    assert_eq!(refs(&r, ReferenceKind::Calls), vec!["dao.search"]);
    let tags = "<cfcomponent>\n  <cffunction name=\"save\">\n    <cfset r = validate(1)>\n    <cfinclude template=\"inc/header.cfm\">\n  </cffunction>\n</cfcomponent>\n";
    let r = extract_file("Saver.cfc", tags, Language::Cfml);
    assert_eq!(names(&r, NodeKind::Method), vec!["Saver::save"]);
    assert_eq!(refs(&r, ReferenceKind::Imports), vec!["inc/header.cfm"]);
    assert_eq!(from(&r, "validate"), "save");
    // A nested tag's `name` is not the function's.
    let nested = "<cfcomponent>\n  <cffunction name=\"load\">\n    <cfquery name=\"q\">SELECT 1</cfquery>\n  </cffunction>\n</cfcomponent>\n";
    let r = extract_file("models/user.dao.cfc", nested, Language::Cfml);
    assert_eq!(names(&r, NodeKind::Method), vec!["user.dao::load"]);
}
