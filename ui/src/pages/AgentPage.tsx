import AgentChat from "../components/AgentChat";
import ErrorBoundary from "../components/ErrorBoundary";

export default function AgentPage() {
  return (
    <ErrorBoundary scope="智能助手" compact>
      <AgentChat variant="page" />
    </ErrorBoundary>
  );
}
