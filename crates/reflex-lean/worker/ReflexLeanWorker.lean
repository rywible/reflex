import Mathlib
import Lean.Data.Json.Stream
import Lean.Util.CollectAxioms

open Lean

namespace ReflexLeanWorker

inductive WireName where
  | anonymous
  | str (parent : WireName) (value : String)
  | num (parent : WireName) (value : Nat)
  deriving FromJson, ToJson, Repr, Nonempty

inductive WireLevel where
  | zero
  | succ (level : WireLevel)
  | max (left right : WireLevel)
  | imax (left right : WireLevel)
  | param (name : WireName)
  deriving FromJson, ToJson, Repr, Nonempty

inductive WireBinderInfo where
  | default
  | implicit
  | strictImplicit
  | instImplicit
  deriving FromJson, ToJson, Repr, Nonempty

inductive WireLiteral where
  | natVal (value : String)
  | strVal (value : String)
  deriving FromJson, ToJson, Repr, Nonempty

inductive WireExpr where
  | bvar (index : Nat)
  | sort (level : WireLevel)
  | const (name : WireName) (levels : List WireLevel)
  | app (function argument : WireExpr)
  | lam (name : WireName) (binderType body : WireExpr) (binderInfo : WireBinderInfo)
  | forallE (name : WireName) (binderType body : WireExpr) (binderInfo : WireBinderInfo)
  | letE (name : WireName) (type value body : WireExpr) (nonDep : Bool)
  | lit (literal : WireLiteral)
  | proj (typeName : WireName) (index : Nat) (subject : WireExpr)
  deriving FromJson, ToJson, Repr, Nonempty

structure VerificationItem where
  proposition : WireExpr
  proofTerm : WireExpr
  allowedAxioms : Array WireName
  deriving FromJson, ToJson

inductive Request where
  | ping (id : UInt64)
  | verify (id : UInt64) (items : Array VerificationItem)
  | index (id : UInt64) (offset limit : Nat)
  | fingerprints (id : UInt64) (offset limit : Nat)
  | fetch (id : UInt64) (names : Array WireName)
  | shutdown (id : UInt64)
  deriving FromJson, ToJson

structure Handshake where
  protocolVersion : Nat
  mathlibCommit : String
  leanToolchain : String
  leanCommit : String
  trustLevel : Nat
  deriving FromJson, ToJson

structure VerificationResult where
  accepted : Bool
  axioms : Array WireName := #[]
  diagnostic : String := ""
  deriving FromJson, ToJson

structure IndexedTheorem where
  name : WireName
  levelParams : Array WireName
  proposition : WireExpr
  proofTerm : WireExpr
  dependencies : Array WireName
  axioms : Array WireName
  deriving FromJson, ToJson

structure TheoremFingerprint where
  name : WireName
  statementHash : UInt64
  dependencies : Array WireName
  deriving FromJson, ToJson

inductive Response where
  | ready (handshake : Handshake)
  | pong (id : UInt64)
  | verified (id : UInt64) (results : Array VerificationResult)
  | indexed (id : UInt64) (total offset : Nat) (artifacts : Array IndexedTheorem)
  | fingerprinted (id : UInt64) (total offset : Nat) (fingerprints : Array TheoremFingerprint)
  | fetched (id : UInt64) (artifacts : Array IndexedTheorem)
  | stopped (id : UInt64)
  | failed (id : UInt64) (diagnostic : String)
  deriving FromJson, ToJson

partial def WireName.toLean : WireName → Name
  | .anonymous => .anonymous
  | .str parent value => .str parent.toLean value
  | .num parent value => .num parent.toLean value

partial def WireName.ofLean : Name → WireName
  | .anonymous => .anonymous
  | .str parent value => .str (ofLean parent) value
  | .num parent value => .num (ofLean parent) value

partial def WireLevel.toLean : WireLevel → Level
  | .zero => .zero
  | .succ level => .succ level.toLean
  | .max left right => .max left.toLean right.toLean
  | .imax left right => .imax left.toLean right.toLean
  | .param name => .param name.toLean

partial def WireLevel.ofLean : Level → Except String WireLevel
  | .zero => pure .zero
  | .succ level => .succ <$> ofLean level
  | .max left right => .max <$> ofLean left <*> ofLean right
  | .imax left right => .imax <$> ofLean left <*> ofLean right
  | .param name => pure (.param (WireName.ofLean name))
  | .mvar _ => throw "unresolved universe metavariable"

def WireBinderInfo.toLean : WireBinderInfo → BinderInfo
  | .default => .default
  | .implicit => .implicit
  | .strictImplicit => .strictImplicit
  | .instImplicit => .instImplicit

def WireBinderInfo.ofLean : BinderInfo → WireBinderInfo
  | .default => .default
  | .implicit => .implicit
  | .strictImplicit => .strictImplicit
  | .instImplicit => .instImplicit

def decodeNat (value : String) : Except String Nat :=
  match value.toNat? with
  | some result => pure result
  | none => throw "invalid natural-number literal"

partial def WireExpr.toLean : WireExpr → Except String Expr
  | .bvar index => pure (.bvar index)
  | .sort level => pure (.sort level.toLean)
  | .const name levels => pure (.const name.toLean (levels.map WireLevel.toLean))
  | .app function argument => .app <$> function.toLean <*> argument.toLean
  | .lam name binderType body binderInfo =>
      .lam name.toLean <$> binderType.toLean <*> body.toLean <*> pure binderInfo.toLean
  | .forallE name binderType body binderInfo =>
      .forallE name.toLean <$> binderType.toLean <*> body.toLean <*> pure binderInfo.toLean
  | .letE name type value body nonDep =>
      .letE name.toLean <$> type.toLean <*> value.toLean <*> body.toLean <*> pure nonDep
  | .lit (.natVal value) => do
      let decoded ← decodeNat value
      pure (.lit (.natVal decoded))
  | .lit (.strVal value) => pure (.lit (.strVal value))
  | .proj typeName index subject => .proj typeName.toLean index <$> subject.toLean

partial def WireExpr.ofLean : Expr → Except String WireExpr
  | .bvar index => pure (.bvar index)
  | .fvar _ => throw "unresolved free variable"
  | .mvar _ => throw "unresolved expression metavariable"
  | .sort level => .sort <$> WireLevel.ofLean level
  | .const name levels => .const (WireName.ofLean name) <$> levels.mapM WireLevel.ofLean
  | .app function argument => .app <$> ofLean function <*> ofLean argument
  | .lam name binderType body binderInfo =>
      .lam (WireName.ofLean name) <$> ofLean binderType <*> ofLean body <*> pure (.ofLean binderInfo)
  | .forallE name binderType body binderInfo =>
      .forallE (WireName.ofLean name) <$> ofLean binderType <*> ofLean body <*> pure (.ofLean binderInfo)
  | .letE name type value body nonDep =>
      .letE (WireName.ofLean name) <$> ofLean type <*> ofLean value <*> ofLean body <*> pure nonDep
  | .lit (.natVal value) => pure (.lit (.natVal (toString value)))
  | .lit (.strVal value) => pure (.lit (.strVal value))
  | .mdata _ expression => ofLean expression
  | .proj typeName index subject => .proj (WireName.ofLean typeName) index <$> ofLean subject

def sortedNames (names : Array Name) : Array Name :=
  names.qsort (Name.quickLt · ·)

partial def visitNameAxioms (env : Environment) (name : Name) : StateM (NameSet × NameSet) Unit := do
  let (seen, found) ← get
  unless seen.contains name do
    set (seen.insert name, found)
    match env.find? name with
    | some (.axiomInfo _) => modify fun (seen, found) => (seen, found.insert name)
    | some info =>
        for dependency in info.type.getUsedConstants do visitNameAxioms env dependency
        if let some value := info.value? then
          for dependency in value.getUsedConstants do visitNameAxioms env dependency
    | none => pure ()

def collectExprAxioms (env : Environment) (expression : Expr) : Array Name := Id.run do
  let ((), (_, found)) := (do
    for name in expression.getUsedConstants do visitNameAxioms env name).run ({}, {})
  return sortedNames found.toArray

partial def visitNameUnsafe (env : Environment) (name : Name) : StateM NameSet Bool := do
  let seen ← get
  if seen.contains name then return false
  set (seen.insert name)
  match env.find? name with
  | none => return true
  | some info =>
      if info.isUnsafe || info.isPartial then return true
      for dependency in info.type.getUsedConstants do
        if ← visitNameUnsafe env dependency then return true
      if let some value := info.value? then
        for dependency in value.getUsedConstants do
          if ← visitNameUnsafe env dependency then return true
      return false

def hasUnsafeDependency (env : Environment) (expression : Expr) : Bool := Id.run do
  let (unsafeFound, _) := (do
    for name in expression.getUsedConstants do
      if ← visitNameUnsafe env name then return true
    return false).run {}
  return unsafeFound

def verifyOne (env : Environment) (item : VerificationItem) : VerificationResult :=
  match item.proposition.toLean, item.proofTerm.toLean with
  | .ok proposition, .ok proofTerm =>
      if proposition.hasSorry || proofTerm.hasSorry then
        { accepted := false, diagnostic := "sorry is forbidden" }
      else if hasUnsafeDependency env proofTerm then
        { accepted := false, diagnostic := "unsafe, partial, or unknown dependency" }
      else
        match Kernel.check env {} proposition, Kernel.check env {} proofTerm with
        | .ok propositionType, .ok proofType =>
            match Kernel.check env {} propositionType with
            | .error _ => { accepted := false, diagnostic := "claim is not a well-formed type" }
            | .ok _ =>
                if !Kernel.isDefEqGuarded env {} proofType proposition then
                  { accepted := false, diagnostic := "proof type differs from the claim" }
                else
                  let axioms := collectExprAxioms env proofTerm
                  let allowed := item.allowedAxioms.map WireName.toLean
                  if axioms.any (· == ``sorryAx) then
                    { accepted := false, diagnostic := "sorryAx dependency is forbidden" }
                  else if axioms.any fun ax => !allowed.contains ax then
                    { accepted := false, diagnostic := "proof introduced a new axiom" }
                  else
                    { accepted := true, axioms := axioms.map WireName.ofLean }
        | .error _, _ => { accepted := false, diagnostic := "kernel rejected the claim" }
        | _, .error _ => { accepted := false, diagnostic := "kernel rejected the proof" }
  | .error diagnostic, _ => { accepted := false, diagnostic }
  | _, .error diagnostic => { accepted := false, diagnostic }

def indexedTheorem? (env : Environment) (name : Name) (includeBody : Bool) : Option IndexedTheorem := do
  let .thmInfo theoremInfo ← env.find? name | none
  if theoremInfo.type.hasSorry || theoremInfo.value.hasSorry then none else
  if hasUnsafeDependency env theoremInfo.value then none else
  let axioms := collectExprAxioms env theoremInfo.value
  if axioms.any (· == ``sorryAx) then none else
  let proposition ← WireExpr.ofLean theoremInfo.type |>.toOption
  let proofTerm? := if includeBody then WireExpr.ofLean theoremInfo.value |>.toOption else
    some (WireExpr.const (WireName.ofLean name)
      (theoremInfo.levelParams.map (fun levelName => WireLevel.param (WireName.ofLean levelName))))
  let proofTerm ← proofTerm?
  let dependencies := sortedNames theoremInfo.value.getUsedConstants
  some {
    name := WireName.ofLean name
    levelParams := theoremInfo.levelParams.toArray.map WireName.ofLean
    proposition
    proofTerm
    dependencies := dependencies.map WireName.ofLean
    axioms := axioms.map WireName.ofLean
  }

def eligibleTheoremNames (env : Environment) : Array Name :=
  env.constants.fold (fun names name info =>
    match info with
    | .thmInfo theoremInfo =>
        if theoremInfo.type.hasSorry || theoremInfo.value.hasSorry then names else names.push name
    | _ => names) #[] |>.qsort (Name.quickLt · ·)

def writeResponse (output : IO.FS.Stream) (response : Response) : IO Unit := do
  output.putStrLn (toJson response).compress
  output.flush

partial def serve (env : Environment) (names : Array Name) (input output : IO.FS.Stream) : IO Unit := do
  let line ← input.getLine
  if line.isEmpty then return
  let request : Request ← IO.ofExcept do
    let json ← Json.parse line
    fromJson? json
  match request with
  | .ping id =>
      writeResponse output (.pong id)
      serve env names input output
  | .verify id items =>
      writeResponse output (.verified id (items.map (verifyOne env)))
      serve env names input output
  | .index id offset limit =>
      let artifacts := (names.extract offset (min names.size (offset + limit))).filterMap
        (indexedTheorem? env · false)
      writeResponse output (.indexed id names.size offset artifacts)
      serve env names input output
  | .fingerprints id offset limit =>
      let fingerprints := (names.extract offset (min names.size (offset + limit))).filterMap fun name =>
        match env.find? name with
        | some (.thmInfo theoremInfo) =>
          let dependencies := Id.run do
            let mut found : NameSet := {}
            for dependency in theoremInfo.type.getUsedConstants do
              found := found.insert dependency
            for dependency in theoremInfo.value.getUsedConstants do
              found := found.insert dependency
            return sortedNames found.toArray
          some {
            name := WireName.ofLean name
            statementHash := hash theoremInfo.type
            dependencies := dependencies.map WireName.ofLean
          }
        | _ => none
      writeResponse output (.fingerprinted id names.size offset fingerprints)
      serve env names input output
  | .fetch id requestedNames =>
      let artifacts := requestedNames.filterMap fun requested =>
        indexedTheorem? env requested.toLean true
      writeResponse output (.fetched id artifacts)
      serve env names input output
  | .shutdown id => writeResponse output (.stopped id)

def run : IO Unit := do
  Lean.initSearchPath (← Lean.findSysroot)
  let options := ({} : Options).setBool `warningAsError true
  let env ← importModules #[{ module := `Mathlib }] options 0
  let input ← IO.getStdin
  let output ← IO.getStdout
  writeResponse output (.ready {
    protocolVersion := 1
    mathlibCommit := "7178aee7a431bb7527da15c3507836d8dfefcda4"
    leanToolchain := "leanprover/lean4:v4.15.0-rc1"
    leanCommit := "ffac974dba799956a97d63ffcb13a774f700149c"
    trustLevel := 0
  })
  serve env (eligibleTheoremNames env) input output

end ReflexLeanWorker

def main : IO Unit := ReflexLeanWorker.run
