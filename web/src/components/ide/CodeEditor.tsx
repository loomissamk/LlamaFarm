import Editor, { type OnMount } from '@monaco-editor/react';
import { useRef } from 'react';
import './monacoSetup';
import { languageForPath } from './languageForPath';

export interface CodeEditorProps {
  path: string;
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  readOnly?: boolean;
}

export default function CodeEditor({ path, value, onChange, onSave, readOnly }: Readonly<CodeEditorProps>) {
  const onSaveRef = useRef(onSave);
  onSaveRef.current = onSave;

  const handleMount: OnMount = (editor, monaco) => {
    editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
      onSaveRef.current();
    });
  };

  return (
    <Editor
      key={path}
      path={path}
      language={languageForPath(path)}
      value={value}
      theme="vs-dark"
      onChange={(next) => onChange(next ?? '')}
      onMount={handleMount}
      options={{
        readOnly,
        minimap: { enabled: true },
        fontSize: 13,
        automaticLayout: true,
        scrollBeyondLastLine: false,
        wordWrap: 'off',
        tabSize: 2,
      }}
    />
  );
}
