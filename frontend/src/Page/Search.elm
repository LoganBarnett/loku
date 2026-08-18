module Page.Search exposing (Model, Msg, init, update, view)

import Api exposing (SearchPage)
import Browser.Navigation as Nav
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (onInput, onSubmit)
import Http
import Route
import Ui


pageSize : Int
pageSize =
    48


type alias Model =
    { key : Nav.Key
    , params : Route.SearchParams
    , input : String
    , results : Results
    }


type Results
    = Loading
    | Loaded SearchPage
    | Failed String


type Msg
    = GotResults (Result Http.Error SearchPage)
    | QueryInput String
    | QuerySubmit
    | GoToPage Int


init : Nav.Key -> Route.SearchParams -> ( Model, Cmd Msg )
init key params =
    ( { key = key, params = params, input = params.query, results = Loading }
    , Api.getSearch
        { query = params.query
        , library = params.library
        , limit = pageSize
        , offset = (params.page - 1) * pageSize
        }
        GotResults
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotResults (Ok page) ->
            ( { model | results = Loaded page }, Cmd.none )

        GotResults (Err err) ->
            ( { model | results = Failed (httpErrorToString err) }, Cmd.none )

        QueryInput input ->
            ( { model | input = input }, Cmd.none )

        QuerySubmit ->
            ( model
            , Nav.pushUrl model.key
                (Route.toString
                    (Route.Search
                        { query = model.input
                        , library = model.params.library
                        , page = 1
                        }
                    )
                )
            )

        GoToPage page ->
            ( model
            , Nav.pushUrl model.key
                (Route.toString
                    (Route.Search
                        { query = model.params.query
                        , library = model.params.library
                        , page = page
                        }
                    )
                )
            )


view : Model -> Html Msg
view model =
    div []
        [ viewHeader model
        , case model.results of
            Loading ->
                p [ class "status-note" ] [ text "Searching…" ]

            Failed err ->
                Ui.errorText err

            Loaded page ->
                viewResults model page
        ]


viewHeader : Model -> Html Msg
viewHeader model =
    div [ class "crumb-bar" ]
        [ a [ href (Route.toString Route.Home) ] [ text "Home" ]
        , Html.form
            [ onSubmit QuerySubmit, class "search-form search-form-header" ]
            [ input
                [ type_ "search"
                , placeholder "Search…"
                , value model.input
                , onInput QueryInput
                , class "search-input"
                ]
                []
            , button [ type_ "submit", class "search-button" ] [ text "Search" ]
            ]
        ]


viewResults : Model -> SearchPage -> Html Msg
viewResults model page =
    let
        totalPages =
            Basics.max 1 ((page.total + pageSize - 1) // pageSize)

        scope =
            case model.params.library of
                Just library ->
                    " in " ++ library

                Nothing ->
                    ""
    in
    div [ class "listing" ]
        [ p [ class "result-summary" ]
            [ text
                (String.fromInt page.total
                    ++ " result(s) for \""
                    ++ model.params.query
                    ++ "\""
                    ++ scope
                )
            ]
        , div [] (List.map Ui.videoCard page.items)
        , if totalPages > 1 then
            Ui.viewPagination GoToPage model.params.page totalPages

          else
            text ""
        ]


httpErrorToString : Http.Error -> String
httpErrorToString err =
    case err of
        Http.BadUrl url ->
            "Bad URL: " ++ url

        Http.Timeout ->
            "Request timed out"

        Http.NetworkError ->
            "Network error"

        Http.BadStatus status ->
            "Server error: " ++ String.fromInt status

        Http.BadBody body ->
            "Unexpected response: " ++ body
