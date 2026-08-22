import Mathlib
import Lean.Data.Json.Stream

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
  levelParams : Array WireName
  claimProposition : WireExpr
  candidateProposition : WireExpr
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
  leanVersion : String
  leanCommit : String
  trustLevel : Nat
  deriving FromJson, ToJson

structure VerificationResult where
  accepted : Bool
  dependencies : Array WireName := #[]
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
  kind : String
  locallyEligible : Bool
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

partial def WireLevel.collectParams : WireLevel → StateM NameSet Unit
  | .zero => pure ()
  | .succ level => level.collectParams
  | .max left right | .imax left right => left.collectParams *> right.collectParams
  | .param name => modify (·.insert name.toLean)

partial def WireExpr.collectLevelParams : WireExpr → StateM NameSet Unit
  | .bvar _ | .lit _ => pure ()
  | .sort level => level.collectParams
  | .const _ levels => levels.forM WireLevel.collectParams
  | .app function argument => function.collectLevelParams *> argument.collectLevelParams
  | .lam _ binderType body _ | .forallE _ binderType body _ =>
      binderType.collectLevelParams *> body.collectLevelParams
  | .letE _ type value body _ =>
      type.collectLevelParams *> value.collectLevelParams *> body.collectLevelParams
  | .proj _ _ subject => subject.collectLevelParams

def VerificationItem.hasUnknownLevelParam (item : VerificationItem) : Bool := Id.run do
  let allowed := item.levelParams.map WireName.toLean
  let ((), found) := (do
    item.claimProposition.collectLevelParams
    item.candidateProposition.collectLevelParams
    item.proofTerm.collectLevelParams).run {}
  return found.toArray.any fun name => !allowed.contains name

def sortedNames (names : Array Name) : Array Name :=
  names.qsort (Name.quickLt · ·)

structure NameAnalysis where
  forbidden : Bool := false
  axioms : Array Name := #[]

abbrev AnalysisCache := IO.Ref (Batteries.HashMap Name NameAnalysis)

def mergeAnalysis (left right : NameAnalysis) : NameAnalysis := Id.run do
  let mut axioms : NameSet := {}
  for name in left.axioms do axioms := axioms.insert name
  for name in right.axioms do axioms := axioms.insert name
  return {
    forbidden := left.forbidden || right.forbidden
    axioms := sortedNames axioms.toArray
  }

partial def analyzeName (env : Environment) (cache : AnalysisCache) (visiting : NameSet)
    (name : Name) : IO NameAnalysis := do
  if let some analysis := (← cache.get).find? name then return analysis
  if visiting.contains name then return {}
  let visiting := visiting.insert name
  let mut analysis : NameAnalysis := {}
  match env.find? name with
  | none => analysis := { forbidden := true }
  | some info =>
      analysis := {
        forbidden := name == ``sorryAx || info.type.hasSorry || info.isUnsafe || info.isPartial
        axioms := match info with | .axiomInfo _ => #[name] | _ => #[]
      }
      for dependency in info.type.getUsedConstants do
        analysis := mergeAnalysis analysis (← analyzeName env cache visiting dependency)
      if let some value := info.value? then
        if value.hasSorry then analysis := { analysis with forbidden := true }
        for dependency in value.getUsedConstants do
          analysis := mergeAnalysis analysis (← analyzeName env cache visiting dependency)
  cache.modify (·.insert name analysis)
  return analysis

def analyzeExpr (env : Environment) (cache : AnalysisCache) (expression : Expr) :
    IO NameAnalysis := do
  let mut analysis : NameAnalysis := { forbidden := expression.hasSorry }
  for name in expression.getUsedConstants do
    analysis := mergeAnalysis analysis (← analyzeName env cache {} name)
  return analysis

def verifyOne (env : Environment) (cache : AnalysisCache) (item : VerificationItem) :
    IO VerificationResult := do
  match item.claimProposition.toLean, item.candidateProposition.toLean, item.proofTerm.toLean with
  | .ok claimProposition, .ok candidateProposition, .ok proofTerm =>
      if claimProposition.hasSorry || candidateProposition.hasSorry || proofTerm.hasSorry then
        return { accepted := false, diagnostic := "sorry is forbidden" }
      else if item.hasUnknownLevelParam then
        return { accepted := false, diagnostic := "proof introduced an unknown universe parameter" }
      else
        let claimAnalysis ← analyzeExpr env cache claimProposition
        let candidateAnalysis ← analyzeExpr env cache candidateProposition
        let proofAnalysis ← analyzeExpr env cache proofTerm
        let analysis := mergeAnalysis claimAnalysis (mergeAnalysis candidateAnalysis proofAnalysis)
        if analysis.forbidden then
          return { accepted := false, diagnostic := "unsafe, partial, sorry, or unknown dependency" }
        let levelParams := (item.levelParams.map WireName.toLean).toList
        let claimDeclaration : Declaration := .axiomDecl {
          name := `_reflex.claim
          levelParams
          type := claimProposition
          isUnsafe := false
        }
        let proofDeclaration : Declaration := .thmDecl {
          name := `_reflex.candidate
          levelParams
          type := candidateProposition
          value := proofTerm
        }
        match env.addDecl {} claimDeclaration with
        | .error _ => return { accepted := false, diagnostic := "claim is not a well-formed type" }
        | .ok _ =>
          match env.addDecl {} proofDeclaration with
          | .error _ => return { accepted := false, diagnostic := "candidate or proof did not kernel-check" }
          | .ok _ =>
            if !Kernel.isDefEqGuarded env {} candidateProposition claimProposition then
              return { accepted := false, diagnostic := "candidate claim differs from the seed claim" }
            else
              let axioms := analysis.axioms
              let allowed := item.allowedAxioms.map WireName.toLean
              if axioms.any (· == ``sorryAx) then
                return { accepted := false, diagnostic := "sorryAx dependency is forbidden" }
              else if axioms.any fun ax => !allowed.contains ax then
                return { accepted := false, diagnostic := "proof introduced a new axiom" }
              else
                let dependencies := Id.run do
                  let mut found : NameSet := {}
                  for expression in #[candidateProposition, proofTerm] do
                    for dependency in expression.getUsedConstants do
                      found := found.insert dependency
                  return sortedNames found.toArray
                let result : VerificationResult := {
                  accepted := true
                  dependencies := dependencies.map WireName.ofLean
                  axioms := axioms.map WireName.ofLean
                }
                return result
  | .error diagnostic, _, _ => return { accepted := false, diagnostic }
  | _, .error diagnostic, _ => return { accepted := false, diagnostic }
  | _, _, .error diagnostic => return { accepted := false, diagnostic }

def indexedTheorem? (env : Environment) (cache : AnalysisCache) (name : Name) :
    IO (Option IndexedTheorem) := do
  let some (.thmInfo theoremInfo) := env.find? name | return none
  let analysis := mergeAnalysis
    (← analyzeExpr env cache theoremInfo.type)
    (← analyzeExpr env cache theoremInfo.value)
  if analysis.forbidden || analysis.axioms.any (· == ``sorryAx) then return none
  let .ok proposition := WireExpr.ofLean theoremInfo.type | return none
  let .ok proofTerm := WireExpr.ofLean theoremInfo.value | return none
  let dependencies := Id.run do
    let mut found : NameSet := {}
    for dependency in theoremInfo.type.getUsedConstants do found := found.insert dependency
    for dependency in theoremInfo.value.getUsedConstants do found := found.insert dependency
    return sortedNames found.toArray
  return some {
    name := WireName.ofLean name
    levelParams := theoremInfo.levelParams.toArray.map WireName.ofLean
    proposition
    proofTerm
    dependencies := dependencies.map WireName.ofLean
    axioms := analysis.axioms.map WireName.ofLean
  }

def eligibleTheoremNames (env : Environment) : Array Name :=
  env.constants.fold (fun names name info =>
    match info with
    | .thmInfo theoremInfo =>
        if theoremInfo.type.hasSorry || theoremInfo.value.hasSorry then names else names.push name
    | _ => names) #[] |>.qsort (Name.quickLt · ·)

def declarationNames (env : Environment) : Array Name :=
  env.constants.fold (fun names name _ => names.push name) #[] |>.qsort (Name.quickLt · ·)

def declarationKind : ConstantInfo → String
  | .axiomInfo _ => "axiom"
  | .defnInfo _ => "definition"
  | .thmInfo _ => "theorem"
  | .opaqueInfo _ => "opaque"
  | .quotInfo _ => "quotient"
  | .inductInfo _ => "inductive"
  | .ctorInfo _ => "constructor"
  | .recInfo _ => "recursor"

def locallyEligible (name : Name) (info : ConstantInfo) : Bool :=
  name != ``sorryAx && !info.type.hasSorry && !info.isUnsafe && !info.isPartial &&
    match info.value? with
    | some value => !value.hasSorry
    | none => true

def writeResponse (output : IO.FS.Stream) (response : Response) : IO Unit := do
  output.putStrLn (toJson response).compress
  output.flush

partial def serve (env : Environment) (cache : AnalysisCache) (theoremNames allNames : Array Name)
    (input output : IO.FS.Stream) : IO Unit := do
  let line ← input.getLine
  if line.isEmpty then return
  let request : Request ← IO.ofExcept do
    let json ← Json.parse line
    fromJson? json
  match request with
  | .ping id =>
      writeResponse output (.pong id)
      serve env cache theoremNames allNames input output
  | .verify id items =>
      writeResponse output (.verified id (← items.mapM (verifyOne env cache)))
      serve env cache theoremNames allNames input output
  | .index id offset limit =>
      let artifacts ←
        (theoremNames.extract offset (min theoremNames.size (offset + limit))).filterMapM
          (indexedTheorem? env cache)
      writeResponse output (.indexed id theoremNames.size offset artifacts)
      serve env cache theoremNames allNames input output
  | .fingerprints id offset limit =>
      let fingerprints := (allNames.extract offset (min allNames.size (offset + limit))).filterMap fun name =>
        match env.find? name with
        | some info =>
          let dependencies := Id.run do
            let mut found : NameSet := {}
            for dependency in info.type.getUsedConstants do
              found := found.insert dependency
            if let some value := info.value? then
              for dependency in value.getUsedConstants do found := found.insert dependency
            return sortedNames found.toArray
          some {
            name := WireName.ofLean name
            statementHash := hash info.type
            dependencies := dependencies.map WireName.ofLean
            kind := declarationKind info
            locallyEligible := locallyEligible name info
          }
        | _ => none
      writeResponse output (.fingerprinted id allNames.size offset fingerprints)
      serve env cache theoremNames allNames input output
  | .fetch id requestedNames =>
      let artifacts ← requestedNames.filterMapM fun requested =>
        indexedTheorem? env cache requested.toLean
      writeResponse output (.fetched id artifacts)
      serve env cache theoremNames allNames input output
  | .shutdown id => writeResponse output (.stopped id)

def run : IO Unit := do
  Lean.initSearchPath (← Lean.findSysroot)
  let options := ({} : Options).setBool `warningAsError true
  let env ← importModules #[{ module := `Mathlib }] options 0
  let input ← IO.getStdin
  let output ← IO.getStdout
  let cache ← IO.mkRef {}
  writeResponse output (.ready {
    protocolVersion := 1
    leanVersion := Lean.versionStringCore
    leanCommit := Lean.githash
    trustLevel := 0
  })
  serve env cache (eligibleTheoremNames env) (declarationNames env) input output

end ReflexLeanWorker

def main : IO Unit := ReflexLeanWorker.run
