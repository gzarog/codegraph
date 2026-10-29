/**
 * Upstream's C/C++ pre-parse passes, each run by the binary under test
 * (`codegraph __ccpp <pass>`), and the two extractors' `preParse` hooks.
 */
import { execFileSync } from 'node:child_process';
import { binary, binaryEnv } from '../../binary';

function pass(name: string, source: string, options: { file?: string; option?: boolean } = {}): string {
  const args = ['__ccpp', name, ...(options.file ? ['--file', options.file] : []), ...(options.option ? ['--option'] : [])];
  return execFileSync(binary(), args, { input: source, env: binaryEnv(), maxBuffer: 1 << 30 }).toString('utf8');
}

/** How a language's code reads around a macro alone on its line (opaque here). */
export interface LoneMacroRules {
  objc: boolean;
}
export const C_LONE_MACROS: LoneMacroRules = { objc: false };
export const OBJC_LONE_MACROS: LoneMacroRules = { objc: true };

export const blankLoneMacroLines = (source: string, rules: LoneMacroRules = C_LONE_MACROS): string =>
  pass('blankLoneMacroLines', source, { option: rules.objc });
export const blankCppAnnotationMacroCalls = (source: string, options: { keepCReturnTypes?: boolean } = {}): string =>
  pass('blankCppAnnotationMacroCalls', source, { option: !!options.keepCReturnTypes });
export const flattenMidStatementConditionals = (source: string): string => pass('flattenMidStatementConditionals', source);
export const rewriteFoundationEnumMacros = (source: string): string => pass('rewriteFoundationEnumMacros', source);
export const rewriteCppAccessMacros = (source: string): string => pass('rewriteCppAccessMacros', source);
export const blankCppExportMacros = (source: string): string => pass('blankCppExportMacros', source);
export const blankCppInlineMacros = (source: string): string => pass('blankCppInlineMacros', source);
export const blankMetalAttributes = (source: string): string => pass('blankMetalAttributes', source);
export const blankCudaConstructs = (source: string): string => pass('blankCudaConstructs', source);
export const blankCppApiPrefixMacros = (source: string): string => pass('blankCppApiPrefixMacros', source);
export const blankCppInlineAnnotationMacros = (source: string): string => pass('blankCppInlineAnnotationMacros', source);
export const blankCppPointerAnnotationMacros = (source: string): string => pass('blankCppPointerAnnotationMacros', source);
export const blankCppTrailingAttributeMacros = (source: string): string => pass('blankCppTrailingAttributeMacros', source);
export const blankCppDeclaratorAttributeMacros = (source: string): string => pass('blankCppDeclaratorAttributeMacros', source);
export const blankCppLeadingAttributeMacros = (source: string): string => pass('blankCppLeadingAttributeMacros', source);
export const blankCLeadingAttrMacros = (source: string): string => pass('blankCLeadingAttrMacros', source);
export const blankCCplusplusGuardBodies = (source: string): string => pass('blankCCplusplusGuardBodies', source);
export const blankCStatementMacroCalls = (source: string): string => pass('blankCStatementMacroCalls', source);
export const blankCSandwichedAnnotations = (source: string): string => pass('blankCSandwichedAnnotations', source);
export const blankCAutoInference = (source: string): string => pass('blankCAutoInference', source);
export const blankCTrailingParamAttrMacros = (source: string): string => pass('blankCTrailingParamAttrMacros', source);
export const blankCKernelAnnotations = (source: string): string => pass('blankCKernelAnnotations', source);
export const blankCParameterizedAnnotationMacros = (source: string): string => pass('blankCParameterizedAnnotationMacros', source);
export const blankCTypeKeywordArgs = (source: string): string => pass('blankCTypeKeywordArgs', source);
export const blankCFileScopePrefixedDeclMacros = (source: string): string => pass('blankCFileScopePrefixedDeclMacros', source);
export const rewriteCPrefixedDeclMacroInitializers = (source: string): string =>
  pass('rewriteCPrefixedDeclMacroInitializers', source);
export const blankCVaArgQualifiedTypeArgs = (source: string): string => pass('blankCVaArgQualifiedTypeArgs', source);
export const blankCNamedVariadicDefineDots = (source: string): string => pass('blankCNamedVariadicDefineDots', source);
export const blankCDesignatedMacroArgs = (source: string): string => pass('blankCDesignatedMacroArgs', source);
export const blankCUnbalancedConditionalBranches = (source: string): string =>
  pass('blankCUnbalancedConditionalBranches', source);
export const stripCppTemplateArgs = (name: string): string => pass('stripCppTemplateArgs', name);
export const recoverMangledCppName = (name: string): string => pass('recoverMangledCppName', name);

/** The extractors, as far as tests reach into them: the pre-parse hook. */
export const cppExtractor = {
  preParse: (source: string, filePath?: string): string => pass('cppPreParse', source, { file: filePath }),
};
export const cExtractor = {
  preParse: (source: string): string => pass('cPreParse', source),
};
