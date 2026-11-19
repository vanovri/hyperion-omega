{{- define "hyperion.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "hyperion.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "hyperion.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: hyperion-exchange
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end -}}

{{- define "hyperion.secretName" -}}
{{- if .Values.secrets.existingSecretName -}}
{{- .Values.secrets.existingSecretName -}}
{{- else -}}
{{- printf "%s-secrets" (include "hyperion.fullname" .) -}}
{{- end -}}
{{- end -}}

{{- define "hyperion.podSecurityContext" -}}
runAsNonRoot: {{ .Values.podSecurityStandards.runAsNonRoot }}
runAsUser: {{ .Values.podSecurityStandards.runAsUser }}
runAsGroup: {{ .Values.podSecurityStandards.runAsGroup }}
fsGroup: {{ .Values.podSecurityStandards.fsGroup }}
seccompProfile:
  type: {{ .Values.podSecurityStandards.seccompProfileType }}
{{- end -}}

{{- define "hyperion.containerSecurityContext" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
runAsNonRoot: true
runAsUser: {{ .Values.podSecurityStandards.runAsUser }}
capabilities:
  drop:
    - ALL
{{- end -}}
